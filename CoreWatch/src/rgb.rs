//! RGB control through the OpenRGB SDK server (default 127.0.0.1:6742).
//!
//! OpenRGB already speaks to hundreds of motherboards, RAM kits, GPUs, fans,
//! strips and peripherals, so we lean on it instead of re-implementing every
//! vendor protocol. A worker thread owns the connection and renders effects
//! at ~30 FPS; the UI only sends it settings.

use std::collections::{HashMap, HashSet};
use std::f32::consts::TAU;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use openrgb::{data::{Color, ColorMode, Mode, ModeFlag}, OpenRGB};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::MissedTickBehavior;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect {
    Off,
    Static,
    Breathing,
    Rainbow,
    Temperature,
}

impl Effect {
    pub const ALL: [Effect; 5] = [
        Effect::Static,
        Effect::Breathing,
        Effect::Rainbow,
        Effect::Temperature,
        Effect::Off,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Effect::Off => "Off",
            Effect::Static => "Static",
            Effect::Breathing => "Breathing",
            Effect::Rainbow => "Rainbow",
            Effect::Temperature => "Temperature",
        }
    }

    pub fn animated(self) -> bool {
        matches!(self, Effect::Breathing | Effect::Rainbow)
    }

    pub fn uses_color(self) -> bool {
        matches!(self, Effect::Static | Effect::Breathing)
    }

    pub fn uses_speed(self) -> bool {
        self.animated()
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct RgbSettings {
    pub effect: Effect,
    pub color: [u8; 3],
    pub speed: f32,
    pub brightness: f32,
    /// Only meaningful for the Temperature effect (whole degrees C).
    pub temp: i32,
    pub devices: Vec<u32>,
}

impl Default for RgbSettings {
    fn default() -> Self {
        Self {
            effect: Effect::Static,
            color: [0, 70, 255],
            speed: 1.0,
            brightness: 1.0,
            temp: 0,
            devices: Vec::new(),
        }
    }
}

pub enum RgbCmd {
    Connect,
    Disconnect,
    Apply(RgbSettings),
    FollowTemperature(i32),
}

#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub index: u32,
    pub name: String,
    pub leds: usize,
}

#[derive(Clone, Default, Debug)]
pub struct RgbStatus {
    pub connected: bool,
    pub devices: Vec<DeviceInfo>,
    pub message: String,
    pub apply_message: String,
    pub apply_failed: bool,
}

pub fn spawn(ctx: eframe::egui::Context) -> (UnboundedSender<RgbCmd>, Arc<Mutex<RgbStatus>>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let status = Arc::new(Mutex::new(RgbStatus {
        message: "Not connected".into(),
        ..Default::default()
    }));
    let st = status.clone();

    std::thread::Builder::new()
        .name("rgb".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(worker(rx, st, ctx));
        })
        .expect("failed to start rgb thread");

    (tx, status)
}

fn set_status(status: &Arc<Mutex<RgbStatus>>, f: impl FnOnce(&mut RgbStatus)) {
    if let Ok(mut s) = status.lock() {
        f(&mut s);
    }
}

async fn connect() -> Result<(OpenRGB<TcpStream>, Vec<DeviceInfo>), String> {
    tokio::time::timeout(Duration::from_secs(6), async {
        let client = OpenRGB::connect().await.map_err(|e| e.to_string())?;
        client.set_name("CoreWatch").await.map_err(|e| e.to_string())?;
        let count = client.get_controller_count().await.map_err(|e| e.to_string())?;
        let mut devices = Vec::new();
        for i in 0..count {
            let c = client.get_controller(i).await.map_err(|e| format!("Device {i}: {e}"))?;
            if !c.leds.is_empty() {
                devices.push(DeviceInfo { index: i, name: c.name, leds: c.leds.len() });
            }
        }
        Ok((client, devices))
    }).await.map_err(|_| "OpenRGB discovery timed out. Check the SDK server and retry.".to_string())?
}

struct PreparedMode { index: i32, mode: Mode }

fn mode_priority(mode: &Mode) -> Option<u8> {
    if !(mode.flags.contains(ModeFlag::HasPerLEDColor) || mode.flags.contains(ModeFlag::HasModeSpecificColor)) { return None; }
    match mode.name.to_ascii_lowercase().as_str() {
        "direct" => Some(0), "custom" => Some(1), "static" => Some(2), _ => None,
    }
}

async fn prepare(client: &OpenRGB<TcpStream>, id: u32) -> Result<PreparedMode, String> {
    let controller = client.get_controller(id).await.map_err(|e| e.to_string())?;
    let (index, mut mode) = controller.modes.into_iter().enumerate()
        .filter_map(|(i, m)| mode_priority(&m).map(|rank| (rank, i, m)))
        .min_by_key(|(rank, _, _)| *rank)
        .map(|(_, i, m)| (i as i32, m))
        .ok_or_else(|| "No Direct, Custom, or Static color mode is exposed by this device".to_string())?;
    // OpenRGB may leave a mode in random-color or zero-brightness state.
    mode.color_mode = Some(if mode.flags.contains(ModeFlag::HasPerLEDColor) {
        ColorMode::PerLED
    } else { ColorMode::ModeSpecific });
    if let Some(max) = mode.brightness_max { mode.brightness = Some(max); }
    client.update_mode(id, index, mode.clone()).await.map_err(|e| e.to_string())?;
    let check = client.get_controller(id).await.map_err(|e| e.to_string())?;
    if check.active_mode != index { return Err("OpenRGB did not switch to the requested color mode".into()); }
    Ok(PreparedMode { index, mode })
}

async fn send_colors(client: &OpenRGB<TcpStream>, id: u32, settings: &RgbSettings,
    t: f32, n: usize, mode: &PreparedMode) -> Result<(), String> {
    if mode.mode.color_mode == Some(ColorMode::ModeSpecific) {
        // Some hardware uses a mode-level color instead of the controller LED buffer.
        let mut updated = mode.mode.clone();
        let count = updated.colors.len().max(updated.colors_min.unwrap_or(1) as usize).max(1);
        updated.colors = (0..count).map(|i| effect_color(settings, t, i, count)).collect();
        client.update_mode(id, mode.index, updated).await.map_err(|e| e.to_string())
    } else {
        let colors = (0..n).map(|i| effect_color(settings, t, i, n)).collect();
        client.update_leds(id, colors).await.map_err(|e| e.to_string())
    }
}

fn color_hex(c: Color) -> String { format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b) }

async fn readback(client: &OpenRGB<TcpStream>, id: u32, settings: &RgbSettings,
    t: f32, n: usize, mode: &PreparedMode) -> Result<(String, bool), String> {
    // A short pause allows the server's hardware-command queue to catch up.
    // Animation does not advance while this first-frame check is in progress.
    let specific = mode.mode.color_mode == Some(ColorMode::ModeSpecific);
    let count = if specific { mode.mode.colors.len().max(mode.mode.colors_min.unwrap_or(1) as usize).max(1) } else { n };
    let expected: Vec<Color> = (0..count).map(|i| effect_color(settings,t,i,count)).collect();
    let mut result = (String::new(), false);
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_millis(40)).await;
        let controller = client.get_controller(id).await.map_err(|e| e.to_string())?;
        let actual = if specific {
            controller.modes.get(mode.index as usize).map(|m| m.colors.as_slice()).unwrap_or(&[])
        } else { controller.colors.as_slice() };
        let matches = actual == expected.as_slice() && controller.active_mode == mode.index;
        let sent = expected.first().copied().map(color_hex).unwrap_or_else(|| "none".into());
        let got = actual.first().copied().map(color_hex).unwrap_or_else(|| "none".into());
        result = (format!("{} · {} · sent {} / server {} · {}", controller.name, mode.mode.name,
            sent, got, if matches { "RGB data matches" } else { "READBACK MISMATCH" }), matches);
        if matches { break; }
    }
    Ok(result)
}

async fn worker(mut rx: UnboundedReceiver<RgbCmd>, status: Arc<Mutex<RgbStatus>>, ctx: eframe::egui::Context) {
    let mut client: Option<OpenRGB<TcpStream>> = None;
    let mut settings: Option<RgbSettings> = None;
    let mut devices: HashMap<u32, DeviceInfo> = HashMap::new();
    let mut prepared: HashMap<u32, PreparedMode> = HashMap::new();
    let mut blocked = HashSet::new();
    let mut dirty = false;
    let mut report = false;
    let mut started = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(33));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            cmd = rx.recv() => {
                match cmd {
                    None => break,
                    Some(RgbCmd::Connect) => {
                        client = None;
                        settings = None;
                        prepared.clear();
                        blocked.clear();
                        set_status(&status, |s| {
                            s.connected = false;
                            s.devices.clear();
                            s.message = "Connecting to OpenRGB…".into();
                            s.apply_message.clear();
                            s.apply_failed = false;
                        });
                        ctx.request_repaint();
                        match connect().await {
                            Ok((c, list)) => {
                                devices = list.iter().map(|d| (d.index, d.clone())).collect();
                                client = Some(c);
                                set_status(&status, |s| {
                                    s.connected = true;
                                    s.message = format!("Connected · {} devices", list.len());
                                    s.devices = list;
                                });
                            }
                            Err(e) => set_status(&status, |s| s.message = e),
                        }
                    }
                    Some(RgbCmd::Disconnect) => {
                        client = None;
                        settings = None;
                        prepared.clear();
                        set_status(&status, |s| {
                            s.connected = false;
                            s.devices.clear();
                            s.message = "Disconnected".into();
                            s.apply_message.clear();
                        });
                    }
                    Some(RgbCmd::Apply(s)) => {
                        started = Instant::now();
                        settings = Some(s);
                        prepared.clear(); // Apply always re-establishes mode, including retries.
                        blocked.clear();
                        dirty = true;
                        report = true;
                        set_status(&status, |s| { s.apply_message = "Applying lighting…".into(); s.apply_failed = false; });
                    }
                    Some(RgbCmd::FollowTemperature(temp)) => {
                        if let Some(s) = settings.as_mut() { s.temp = temp; dirty = true; }
                    }
                }
                ctx.request_repaint();
            }
            _ = tick.tick() => {
                let (Some(c), Some(s)) = (client.as_ref(), settings.as_ref()) else { continue };
                if !(dirty || s.effect.animated()) { continue; }
                dirty = false;
                let mut sent = 0;
                let mut readbacks = Vec::new();
                let mut readback_mismatch = false;
                let mut failures = Vec::new();
                let mut timed_out = false;
                for &id in &s.devices {
                    if blocked.contains(&id) { continue; }
                    let Some(d) = devices.get(&id) else {
                        failures.push(format!("Device {id} is no longer available; rescan"));
                        blocked.insert(id);
                        continue;
                    };
                    let result = tokio::time::timeout(Duration::from_secs(2), async {
                        if !prepared.contains_key(&id) { prepared.insert(id, prepare(c, id).await?); }
                        let t = started.elapsed().as_secs_f32();
                        send_colors(c, id, s, t, d.leds, &prepared[&id]).await?;
                        if report { readback(c,id,s,t,d.leds,&prepared[&id]).await.map(Some) }
                        else { Ok(None) }
                    }).await;
                    match result {
                        Ok(Ok(detail)) => {
                            sent += 1;
                            if let Some((text, matches)) = detail { readback_mismatch |= !matches; readbacks.push(text); }
                        },
                        Ok(Err(e)) => {
                            blocked.insert(id);
                            let local_mode_error = e.starts_with("No Direct, Custom, or Static");
                            failures.push(format!("{}: {e}", d.name));
                            if !local_mode_error { timed_out = true; break; }
                        }
                        Err(_) => { timed_out = true; failures.push(format!("{}: timed out; reconnect before retrying", d.name)); break; }
                    }
                }
                if timed_out {
                    client = None;
                    set_status(&status, |st| { st.connected = false; st.message = "SDK communication failed. Click Connect, then Apply to retry.".into(); });
                }
                if report || !failures.is_empty() {
                    set_status(&status, |st| {
                        st.apply_failed = !failures.is_empty() || readback_mismatch;
                        st.apply_message = if failures.is_empty() {
                            if readback_mismatch { format!("Server data did not match the request.\n{}", readbacks.join("\n")) } else { format!("Lighting applied to {sent} device(s).") }
                        } else { format!("Sent to {sent} device(s). {}", failures.join(" | ")) };
                    });
                    report = false;
                    ctx.request_repaint();
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Effects
// ---------------------------------------------------------------------------

fn effect_color(s: &RgbSettings, t: f32, i: usize, n: usize) -> Color {
    let base = [
        s.color[0] as f32 / 255.0,
        s.color[1] as f32 / 255.0,
        s.color[2] as f32 / 255.0,
    ];

    let (r, g, b) = match s.effect {
        Effect::Off => (0.0, 0.0, 0.0),
        Effect::Static => (base[0], base[1], base[2]),
        Effect::Breathing => {
            let wave = 0.5 + 0.5 * (t * s.speed * TAU * 0.4).cos();
            let k = 0.04 + 0.96 * wave;
            (base[0] * k, base[1] * k, base[2] * k)
        }
        Effect::Rainbow => {
            let h = (t * 0.12 * s.speed + i as f32 / n.max(1) as f32).fract();
            hsv(h, 1.0, 1.0)
        }
        Effect::Temperature => {
            // 30 °C = blue ... 90 °C = red
            let k = ((s.temp as f32 - 30.0) / 60.0).clamp(0.0, 1.0);
            hsv(0.62 * (1.0 - k), 1.0, 1.0)
        }
    };

    let br = s.brightness.clamp(0.0, 1.0);
    Color::new(
        (r * br * 255.0).round() as u8,
        (g * br * 255.0).round() as u8,
        (b * br * 255.0).round() as u8,
    )
}

fn hsv(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let h6 = (h.fract() + 1.0).fract() * 6.0;
    let i = h6.floor() as i32;
    let f = h6 - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match i % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    fn decode_mode(payload: &[u8]) -> (u32,u32,Vec<[u8;4]>) {
        let name_len = u16::from_le_bytes(payload[8..10].try_into().unwrap()) as usize;
        let fields = 10 + name_len;
        let read = |i: usize| u32::from_le_bytes(payload[fields+i*4..fields+i*4+4].try_into().unwrap());
        let count_at = fields + 48;
        let count = u16::from_le_bytes(payload[count_at..count_at+2].try_into().unwrap()) as usize;
        let colors = payload[count_at+2..].chunks_exact(4).take(count).map(|c| c.try_into().unwrap()).collect();
        (read(9),read(11),colors)
    }

    fn string(out: &mut Vec<u8>, s: &str) { out.extend_from_slice(&((s.len()+1) as u16).to_le_bytes()); out.extend_from_slice(s.as_bytes()); out.push(0); }
    fn u32s(out: &mut Vec<u8>, values: &[u32]) { for v in values { out.extend_from_slice(&v.to_le_bytes()); } }
    fn controller(specific: bool) -> Vec<u8> { controller_color(specific, [0,0,0,0]) }
    fn controller_color(specific: bool, color: [u8;4]) -> Vec<u8> {
        let mut b = Vec::new();
        u32s(&mut b, &[3]);
        for s in ["Test controller", "Test", "Fixture", "1", "0", "loopback"] { string(&mut b,s); }
        b.extend_from_slice(&1u16.to_le_bytes()); u32s(&mut b,&[0]);
        string(&mut b,if specific {"Static"} else {"Direct"});
        u32s(&mut b,&[0, (if specific {64} else {32}) | 16 | 1024, 0,0,0,100,1,1,0,0,0,3]);
        b.extend_from_slice(&1u16.to_le_bytes()); b.extend_from_slice(&color);
        b.extend_from_slice(&0u16.to_le_bytes()); // zones
        b.extend_from_slice(&1u16.to_le_bytes()); string(&mut b,"LED"); u32s(&mut b,&[0]);
        b.extend_from_slice(&1u16.to_le_bytes()); b.extend_from_slice(&color);
        // Model a newer server appending metadata unknown to the old decoder.
        b.extend_from_slice(&[0xAA,0xBB,0xCC,0xDD]);
        let mut out=Vec::new(); u32s(&mut out,&[(b.len()+4) as u32]); out.extend(b); out
    }
    async fn packet(s: &mut TcpStream) -> (u32,Vec<u8>) {
        let mut h=[0u8;16]; s.read_exact(&mut h).await.unwrap(); assert_eq!(&h[..4],b"ORGB");
        let id=u32::from_le_bytes(h[8..12].try_into().unwrap());
        let n=u32::from_le_bytes(h[12..16].try_into().unwrap()) as usize;
        let mut b=vec![0;n]; s.read_exact(&mut b).await.unwrap();
        if id == 1050 || id == 1101 {
            let declared=u32::from_le_bytes(b[..4].try_into().unwrap()) as usize;
            // OpenRGB's server checks that this includes the size field itself.
            assert_eq!(declared,n,"OpenRGB rejects mismatched inner packet size");
        }
        (id,b)
    }
    async fn response(s: &mut TcpStream,id:u32,b:&[u8]) {
        let mut h=b"ORGB".to_vec(); u32s(&mut h,&[0,id,b.len() as u32]); h.extend_from_slice(b); s.write_all(&h).await.unwrap();
    }
    async fn exercise(specific:bool, mismatch:bool) {
        let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr=listener.local_addr().unwrap();
        let server=tokio::spawn(async move {
            let (mut socket,_)=listener.accept().await.unwrap();
            assert_eq!(packet(&mut socket).await.0,40); response(&mut socket,40,&6u32.to_le_bytes()).await;
            for _ in 0..2 { // repeated Apply must resend mode and color, even for identical settings
                assert_eq!(packet(&mut socket).await.0,1); response(&mut socket,1,&controller(specific)).await;
                let (id,payload)=packet(&mut socket).await; assert_eq!(id,1101);
                let mode=decode_mode(&payload);
                assert_eq!(mode.0,100);
                assert_eq!(mode.1,if specific {2} else {1});
                assert_eq!(packet(&mut socket).await.0,1); response(&mut socket,1,&controller(specific)).await;
                let (id,payload)=packet(&mut socket).await;
                if specific {
                    assert_eq!(id,1101);
                    let mode=decode_mode(&payload);
                    assert_eq!(mode.2,vec![[255,70,0,0]]);
                } else {
                    assert_eq!(id,1050);
                    assert_eq!(&payload[4..],&[1,0,255,70,0,0]);
                }
                for _ in 0..if mismatch {3} else {1} {
                    assert_eq!(packet(&mut socket).await.0,1);
                    response(&mut socket,1,&controller_color(specific,if mismatch {[255,255,255,0]} else {[255,70,0,0]})).await;
                }
            }
        });
        let client=OpenRGB::connect_to(addr).await.unwrap();
        for effect in [Effect::Static, Effect::Breathing] {
            let settings=RgbSettings {color:[255,70,0],effect,..Default::default()};
            let mode=prepare(&client,0).await.unwrap();
            send_colors(&client,0,&settings,0.0,1,&mode).await.unwrap();
            let (report,matches)=readback(&client,0,&settings,0.0,1,&mode).await.unwrap();
            assert_eq!(matches,!mismatch);
            assert!(report.contains("sent #FF4600"));
            assert!(report.contains(if mismatch {"READBACK MISMATCH"} else {"RGB data matches"}));
        }
        server.await.unwrap();
    }
    #[tokio::test]
    async fn readback_detects_server_retaining_white() { tokio::time::timeout(Duration::from_secs(5),exercise(false,true)).await.unwrap(); }
    #[tokio::test]
    async fn direct_mode_and_retry_send_expected_color() { tokio::time::timeout(Duration::from_secs(5),exercise(false,false)).await.unwrap(); }
    #[tokio::test]
    async fn mode_specific_hardware_receives_color_in_mode_packet() { tokio::time::timeout(Duration::from_secs(5),exercise(true,false)).await.unwrap(); }
}

#[cfg(test)]
mod breathing_tests {
    use super::*;
    #[test]
    fn breathing_preserves_saturated_channels_through_multiple_cycles() {
        for color in [[255,0,0], [0,255,0], [0,0,255]] {
            let s = RgbSettings { effect: Effect::Breathing, color, ..Default::default() };
            assert_eq!(effect_color(&s,0.0,0,1),Color::new(color[0],color[1],color[2]));
            for i in 0..1000 {
                let c=effect_color(&s,i as f32 * 0.033,0,1);
                for (base,value) in color.into_iter().zip([c.r,c.g,c.b]) {
                    if base==0 {assert_eq!(value,0);} else {assert!(value>=10);}
                }
            }
        }
    }
}
