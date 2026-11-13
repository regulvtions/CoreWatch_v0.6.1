//! Optional fan control via LibreHardwareMonitor's LOCAL HTTP server.
//! This deliberately does not drive the motherboard directly. Overrides are opt-in.
//! Only control sensors returned by LHM are offered; pumps are excluded.
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex, mpsc::{self, Sender, Receiver}};
use std::time::{Duration, Instant};
use serde_json::Value;
use crate::sensors::Snapshot;

const HOST: &str = "127.0.0.1:8085";
const MIN_DUTY: f32 = 35.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Profile { Quiet, Balanced, Performance, Custom }
impl Profile {
    pub const ALL: [Self; 4] = [Self::Quiet, Self::Balanced, Self::Performance, Self::Custom];
    pub fn name(self) -> &'static str { match self { Self::Quiet => "Quiet", Self::Balanced => "Balanced", Self::Performance => "Performance", Self::Custom => "Custom" } }
    pub fn points(self) -> [(f32, f32); 4] { match self {
        Self::Quiet => [(30., 35.), (50., 40.), (70., 65.), (85., 100.)],
        Self::Balanced => [(30., 35.), (50., 50.), (70., 75.), (85., 100.)],
        Self::Performance => [(30., 55.), (50., 70.), (70., 90.), (85., 100.)],
        Self::Custom => [(30., 35.), (50., 50.), (70., 75.), (85., 100.)],
    }}
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source { Cpu, GpuMax }
#[derive(Clone, Debug)]
pub enum Mode { Automatic, Manual(f32), Curve { profile: Profile, source: Source, points: [(f32,f32);4] } }
#[derive(Clone, Debug)]
pub struct FanDevice { pub id: String, pub name: String, pub mode: Mode, pub last_duty: Option<f32> }
#[derive(Clone, Default, Debug)]
pub struct FanStatus { pub online: bool, pub message: String, pub fans: Vec<FanDevice> }
pub enum Command { SetMode(String, Mode), SetMany(Vec<String>, Mode), ResetAll, Refresh, Shutdown }

pub fn spawn(snapshot: Arc<Mutex<Snapshot>>, ctx: eframe::egui::Context) -> (Sender<Command>, Arc<Mutex<FanStatus>>) {
    let (tx, rx) = mpsc::channel();
    let status = Arc::new(Mutex::new(FanStatus { message: "Enable LibreHardwareMonitor's local HTTP server (port 8085).".into(), ..Default::default() }));
    let out = status.clone();
    std::thread::Builder::new().name("fans".into()).spawn(move || worker(rx, out, snapshot, ctx)).expect("fan control thread");
    (tx, status)
}

// Small localhost-only HTTP client, so no additional TLS or networking dependencies are needed.
pub(crate) fn request(method: &str, path: &str) -> Result<Vec<u8>, String> {
    let addr = HOST.to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("no loopback address")?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(450)).map_err(|e| format!("Local LHM server unavailable: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_millis(900))).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(Duration::from_millis(900))).map_err(|e| e.to_string())?;
    write!(stream, "{method} {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Length: 0\r\n\r\n").map_err(|e| e.to_string())?;
    let mut data = Vec::new();
    stream.take(1_500_000).read_to_end(&mut data).map_err(|e| e.to_string())?;
    let split = data.windows(4).position(|w| w == b"\r\n\r\n").ok_or("Invalid HTTP reply")?;
    let header = String::from_utf8_lossy(&data[..split]);
    if !header.lines().next().unwrap_or("").contains(" 200 ") { return Err(format!("HTTP error: {}", header.lines().next().unwrap_or("unknown"))); }
    Ok(data[split+4..].to_vec())
}
fn encode(s: &str) -> String {
    let mut result = String::new();
    for b in s.bytes() { if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { result.push(b as char); } else { result.push_str(&format!("%{b:02X}")); } }
    result
}
fn set(id: &str, duty: Option<f32>) -> Result<(), String> {
    let val = duty.map(|v| format!("{:.1}", v.clamp(MIN_DUTY,100.))).unwrap_or_else(|| "null".into());
    let path = format!("/Sensor?action=Set&id={}&value={}", encode(id), val);
    let body = request("POST", &path)?;
    let value: Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    if value.get("result").and_then(Value::as_str) != Some("ok") { return Err(value.get("message").and_then(Value::as_str).unwrap_or("Control rejected").to_string()); }
    Ok(())
}
fn collect(v: &Value, out: &mut Vec<(String,String)>) {
    if let Some(obj) = v.as_object() {
        let kind = obj.get("Type").and_then(Value::as_str).unwrap_or("");
        let id = obj.get("SensorId").and_then(Value::as_str).unwrap_or("");
        let name = obj.get("Text").and_then(Value::as_str).unwrap_or("Fan control");
        if kind.eq_ignore_ascii_case("Control") && !id.is_empty() && !name.to_lowercase().contains("pump") && !id.to_lowercase().contains("pump") { out.push((id.into(), name.into())); }
        if let Some(ch) = obj.get("Children").and_then(Value::as_array) { for child in ch { collect(child, out); } }
    }
}
fn discover() -> Result<Vec<(String,String)>, String> {
    let body = request("GET", "/data.json")?;
    let v: Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    let mut found = Vec::new(); collect(&v, &mut found);
    let mut seen = HashSet::new(); found.retain(|(id,_)| seen.insert(id.clone()));
    Ok(found)
}
fn curve(temp: f32, points: &[(f32,f32);4]) -> f32 {
    if temp <= points[0].0 { return points[0].1.clamp(MIN_DUTY, 100.); }
    for window in points.windows(2) {
        if temp <= window[1].0 {
            let span = (window[1].0-window[0].0).max(1.);
            return (window[0].1 + ((temp-window[0].0)/span)*(window[1].1-window[0].1)).clamp(MIN_DUTY,100.);
        }
    }
    points[3].1.clamp(MIN_DUTY,100.)
}
fn reset_owned(owned: &mut HashSet<String>) {
    for id in owned.drain() { let _ = set(&id, None); }
}
fn worker(rx: Receiver<Command>, status: Arc<Mutex<FanStatus>>, snap: Arc<Mutex<Snapshot>>, ctx: eframe::egui::Context) {
    let mut controls: HashMap<String, Mode> = HashMap::new();
    let mut last_duty: HashMap<String, f32> = HashMap::new();
    let mut owned = HashSet::new();
    let mut found: Vec<(String,String)> = Vec::new();
    let mut last_scan = Instant::now() - Duration::from_secs(10);
    let mut last_tick = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(Command::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => { reset_owned(&mut owned); break; }
            Ok(Command::ResetAll) => { reset_owned(&mut owned); controls.clear(); last_duty.clear(); }
            Ok(Command::Refresh) => last_scan = Instant::now() - Duration::from_secs(10),
            Ok(Command::SetMany(ids, mode)) => {
                for id in ids {
                    if found.iter().any(|(known, _)| known == &id) && matches!(mode, Mode::Curve { .. }) {
                        controls.insert(id, mode.clone());
                    }
                }
            }
            Ok(Command::SetMode(id, mode)) => {
                if found.iter().any(|(f,_)| f == &id) {
                    if matches!(mode, Mode::Automatic) {
                        // Explicitly restore even if a previous operation failed.
                        match set(&id, None) {
                            Ok(()) => { controls.remove(&id); owned.remove(&id); last_duty.remove(&id); }
                            Err(e) => { if let Ok(mut s) = status.lock() { s.message = format!("Couldn't restore {id}: {e}"); } }
                        }
                    } else { controls.insert(id, mode); }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if last_scan.elapsed() >= Duration::from_secs(5) {
            last_scan = Instant::now();
            match discover() {
                Ok(list) => { found = list; if let Ok(mut s) = status.lock() { s.online = true; s.message = if found.is_empty() { "Connected: no supported fan control sensors (pumps excluded).".into() } else { format!("Connected: {} available controls. Overrides are opt-in.", found.len()) }; } }
                Err(e) => {
                    // Fail safe: if server disappears, forget overrides; hardware/driver may
                    // retain their last PWM value, so alert user to check the system.
                    if let Ok(mut s) = status.lock() { s.online = false; s.message = format!("{e}. Check fan speeds / restore in LHM or BIOS."); }
                    found.clear();
                }
            }
        }
        if last_tick.elapsed() >= Duration::from_secs(1) {
            last_tick = Instant::now();
            let snapshot = snap.lock().map(|s| s.clone()).unwrap_or_default();
            let mut failed = None;
            for (id, mode) in &controls {
                if !found.iter().any(|(fid,_)| fid == id) { continue; }
                let target = match mode {
                    Mode::Automatic => continue,
                    Mode::Manual(v) => *v,
                    Mode::Curve { source, points, .. } => {
                        let t = match source {
                            Source::Cpu => snapshot.cpu.temp,
                            Source::GpuMax => snapshot.gpus.iter().filter_map(|g| g.temp).reduce(f32::max),
                        };
                        // No usable reading: switch to full speed, never guess a cool temperature.
                        t.map(|v| curve(v, points)).unwrap_or(100.)
                    }
                }.clamp(MIN_DUTY,100.);
                if last_duty.get(id).map_or(true, |v| (target-*v).abs() >= 0.5) {
                    if let Err(e) = set(id, Some(target)) { failed = Some(format!("Fan control error: {e}")); }
                    else { last_duty.insert(id.clone(), target); owned.insert(id.clone()); }
                }
            }
            if let Some(e) = failed { if let Ok(mut s) = status.lock() { s.message = e; } }
        }
        if let Ok(mut s) = status.lock() {
            s.fans = found.iter().map(|(id,name)| FanDevice { id: id.clone(), name: name.clone(), mode: controls.get(id).cloned().unwrap_or(Mode::Automatic), last_duty: last_duty.get(id).copied() }).collect();
        }
        ctx.request_repaint();
    }
}
