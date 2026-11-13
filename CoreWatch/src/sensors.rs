//! Read LibreHardwareMonitor 0.9.6 sensors through its local HTTP JSON API.
//! AMD Radeon readings come from LHM (no NVIDIA driver required).
//! NVIDIA GPU core readings still come from NVML; its hot-spot temperature
//! is supplemented from LibreHardwareMonitor when available.

use std::collections::{BTreeMap};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
use nvml_wrapper::Nvml;
use serde_json::Value;
use sysinfo::System;
#[cfg(windows)]
use wmi::{COMLibrary, WMIConnection};
#[cfg(windows)]
use serde::Deserialize;

const POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Default, Debug)]
pub struct Reading { pub label: String, pub value: f32 }

#[derive(Clone, Default, Debug)]
pub struct CpuData {
    pub name: String,
    pub temp: Option<f32>,
    pub load: Option<f32>,
    pub source: &'static str,
}

#[derive(Clone, Default, Debug)]
pub struct GpuData {
    pub name: String,
    pub temp: Option<f32>,
    pub hotspot: Option<f32>,
    pub fan_pct: Option<f32>,
    pub fan_rpm: Option<f32>,
    pub load: Option<f32>,
    pub source: &'static str,
}

#[derive(Clone, Default, Debug)]
pub struct Snapshot {
    pub seq: u64,
    pub cpu: CpuData,
    pub gpus: Vec<GpuData>,
    pub fans: Vec<Reading>,
    pub nvml_online: bool,
    pub lhm_online: bool,
}

#[cfg(windows)]
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct AcpiZone { current_temperature: u32 }

// The JSON response is a tree with sensor leaf nodes, not a WMI-style table.
// Collect leaf values and keep each leaf's owning hardware name for GPU display.
#[derive(Debug)]
struct Sensor {
    id: String,
    name: String,
    kind: String,
    value: f32,
    hardware_name: String,
}

fn parse_numeric(text: &str) -> Option<f32> {
    text.trim().split_whitespace().next()?.replace(',', ".").parse::<f32>().ok()
        .filter(|v| v.is_finite())
}

fn sensor_value(raw: &Value) -> Option<f32> {
    match raw {
        Value::String(text) => parse_numeric(text),
        Value::Number(n) => n.as_f64().map(|n| n as f32).filter(|v| v.is_finite()),
        _ => None,
    }
}

fn collect(node: &Value, hardware: &str, out: &mut Vec<Sensor>) {
    let hardware = node.get("HardwareId").and_then(Value::as_str)
        .and_then(|_| node.get("Text").and_then(Value::as_str))
        .unwrap_or(hardware);
    if let (Some(id), Some(kind), Some(name), Some(raw)) = (
        node.get("SensorId").and_then(Value::as_str),
        node.get("Type").and_then(Value::as_str),
        node.get("Text").and_then(Value::as_str),
        node.get("RawValue").or_else(|| node.get("Value")),
    ) {
        if let Some(value) = sensor_value(raw) {
            out.push(Sensor { id: id.into(), name: name.into(), kind: kind.into(), value, hardware_name: hardware.into() });
        }
    }
    if let Some(children) = node.get("Children").and_then(Value::as_array) {
        for child in children { collect(child, hardware, out); }
    }
}

fn read_http_sensors() -> Result<Vec<Sensor>, String> {
    // Reuse the same localhost-only HTTP transport as the fan worker.
    let bytes = crate::fans::request("GET", "/data.json")?;
    let root: Value = serde_json::from_slice(&bytes).map_err(|e| format!("Invalid LHM JSON: {e}"))?;
    let mut output = Vec::new();
    collect(&root, "", &mut output);
    if output.is_empty() { return Err("LHM returned no sensor values".into()); }
    Ok(output)
}

fn add_http_readings(
    sensors: &[Sensor],
    cpu: &mut CpuData,
    gpus: &mut Vec<GpuData>,
    fans: &mut Vec<Reading>,
) {
    // Prefer the actual CPU package. Do not mistakenly treat "Distance to TjMax"
    // as a temperature: its value increases as the CPU gets cooler.
    let cpu_temps = sensors.iter()
        .filter(|s| s.kind == "Temperature" && (s.id.starts_with("/intelcpu/") || s.id.starts_with("/amdcpu/")))
        .filter(|s| !s.name.to_ascii_lowercase().contains("distance to"))
        .min_by_key(|s| {
            let n = s.name.to_ascii_lowercase();
            if n == "cpu package" { 0 }
            else if n == "tctl/tdie" || n == "cpu (tctl/tdie)" { 1 }
            else if n == "core max" { 2 }
            else if n == "core average" { 3 }
            else { 4 }
        });
    // Some motherboards report a CPU Core temperature even if their CPU-specific
    // sensor class is unavailable.
    let motherboard_cpu = sensors.iter().find(|s| {
        s.kind == "Temperature" && s.id.starts_with("/lpc/") && s.name.eq_ignore_ascii_case("CPU Core")
    });
    if let Some(s) = cpu_temps.or(motherboard_cpu) {
        cpu.temp = Some(s.value);
        cpu.source = "LibreHardwareMonitor HTTP";
    }

    let mut non_nvidia = BTreeMap::<String, GpuData>::new();
    for s in sensors {
        if s.kind == "Fan" && s.id.starts_with("/lpc/") {
            // Disambiguate identical "Fan #1" labels if multiple controllers exist.
            let controller = s.id.split('/').take(4).collect::<Vec<_>>().join("/");
            fans.push(Reading { label: format!("{} ({})", s.name, controller), value: s.value });
        }
        if !s.id.starts_with("/gpu-") { continue; }
        let parts: Vec<&str> = s.id.split('/').collect();
        if parts.len() < 5 { continue; }
        if !matches!(parts[1], "gpu-amd" | "gpu-nvidia" | "gpu-intel") { continue; }
        let key = format!("/{}/{}", parts[1], parts[2]);
        let name = s.name.to_ascii_lowercase();
        if parts[1] == "gpu-nvidia" {
            let idx = gpus.iter().position(|g| g.name == s.hardware_name)
                .or_else(|| if gpus.len() == 1 { Some(0) } else { None });
            if let Some(idx) = idx {
                if s.kind == "Temperature" && matches!(name.as_str(), "gpu hot spot" | "gpu hotspot") && (0.0..=150.0).contains(&s.value) {
                    gpus[idx].hotspot = Some(s.value);
                }
            }
            continue;
        }
        let g = non_nvidia.entry(key).or_insert_with(|| GpuData {
            name: if s.hardware_name.is_empty() { format!("{} GPU {}", if parts[1] == "gpu-amd" { "AMD Radeon" } else { "Intel" }, parts[2]) } else { s.hardware_name.clone() },
            source: if parts[1] == "gpu-amd" { "AMD · LibreHardwareMonitor" } else { "Intel · LibreHardwareMonitor" }, ..Default::default()
        });
        match s.kind.as_str() {
            "Temperature" if matches!(name.as_str(), "gpu hot spot" | "gpu hotspot") && (0.0..=150.0).contains(&s.value) => g.hotspot = Some(s.value),
            "Temperature" if name == "gpu core" && (0.0..=150.0).contains(&s.value) => g.temp = Some(s.value),
            "Load" if name == "gpu core" && (0.0..=100.0).contains(&s.value) => g.load = Some(s.value),
            "Fan" if s.value >= 0.0 => g.fan_rpm = Some(s.value),
            "Control" if name.contains("fan") && (0.0..=100.0).contains(&s.value) => g.fan_pct = Some(s.value),
            _ => {}
        }
    }
    gpus.extend(non_nvidia.into_values());
}

pub fn spawn(shared: Arc<Mutex<Snapshot>>, ctx: eframe::egui::Context) {
    thread::Builder::new().name("sensors".into())
        .spawn(move || run(shared, ctx)).expect("failed to start sensor thread");
}

fn run(shared: Arc<Mutex<Snapshot>>, ctx: eframe::egui::Context) {
    // ACPI remains a last-resort CPU fallback, but the supported LHM source is HTTP.
    #[cfg(windows)]
    let com = COMLibrary::new().ok();
    #[cfg(windows)]
    let acpi = com.as_ref().and_then(|c| WMIConnection::with_namespace_path("ROOT\\WMI", c.clone()).ok());
    let nvml = Nvml::init().ok();
    let mut sys = System::new();
    sys.refresh_cpu_all();
    let cpu_name = sys.cpus().first().map(|c| c.brand().trim().to_string())
        .filter(|s| !s.is_empty()).unwrap_or_else(|| "CPU".to_string());
    let mut seq = 0u64;
    loop {
        let started = Instant::now();
        let mut gpus = Vec::<GpuData>::new();
        if let Some(n) = &nvml {
            if let Ok(count) = n.device_count() {
                for i in 0..count {
                    if let Ok(d) = n.device_by_index(i) {
                        gpus.push(GpuData {
                            name: d.name().unwrap_or_else(|_| format!("NVIDIA GPU {i}")),
                            temp: d.temperature(TemperatureSensor::Gpu).ok().map(|t| t as f32),
                            fan_pct: d.fan_speed(0).ok().map(|f| f as f32),
                            load: d.utilization_rates().ok().map(|u| u.gpu as f32),
                            source: "NVML", ..Default::default()
                        });
                    }
                }
            }
        }
        sys.refresh_cpu_usage();
        let mut cpu = CpuData { name: cpu_name.clone(), load: Some(sys.global_cpu_usage()), ..Default::default() };
        let mut fans = Vec::new();
        let lhm_online = match read_http_sensors() {
            Ok(sensors) => { add_http_readings(&sensors, &mut cpu, &mut gpus, &mut fans); true }
            Err(_) => false,
        };
        #[cfg(windows)]
        if cpu.temp.is_none() {
            if let Some(conn) = &acpi {
                if let Ok(zones) = conn.raw_query::<AcpiZone>("SELECT CurrentTemperature FROM MSAcpi_ThermalZoneTemperature") {
                    cpu.temp = zones.iter().map(|z| z.current_temperature as f32 / 10.0 - 273.15)
                        .filter(|t| *t > 0.0 && *t < 120.0).reduce(f32::max);
                    if cpu.temp.is_some() { cpu.source = "ACPI thermal zone (approximate)"; }
                }
            }
        }
        fans.sort_by(|a, b| a.label.cmp(&b.label));
        seq += 1;
        if let Ok(mut out) = shared.lock() {
            *out = Snapshot { seq, cpu, gpus, fans, nvml_online: nvml.is_some(), lhm_online };
        }
        ctx.request_repaint();
        let elapsed = started.elapsed();
        if elapsed < POLL_INTERVAL { thread::sleep(POLL_INTERVAL - elapsed); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn card(id: &str, name: &str, core: Value) -> Value {
        json!({"HardwareId":id,"Text":name,"Children":[{"Text":"Temperatures","Children":[
            {"SensorId":format!("{id}/temperature/0"),"Type":"Temperature","Text":"GPU Core","RawValue":core},
            {"SensorId":format!("{id}/temperature/7"),"Type":"Temperature","Text":"GPU Hot Spot","Value":"72,5 °C"},
            {"SensorId":format!("{id}/temperature/1"),"Type":"Temperature","Text":"GPU Memory","Value":"88 °C"}
        ]},
        {"SensorId":format!("{id}/load/0"),"Type":"Load","Text":"GPU Core","RawValue":42.5},
        {"SensorId":format!("{id}/fan/0"),"Type":"Fan","Text":"GPU Fan","Value":"0 RPM"},
        {"SensorId":format!("{id}/control/0"),"Type":"Control","Text":"GPU Fan","RawValue":0.0}]})
    }
    fn readings(root: Value, mut gpus: Vec<GpuData>) -> Vec<GpuData> {
        let mut sensors=Vec::new(); collect(&root,"",&mut sensors);
        add_http_readings(&sensors,&mut CpuData::default(),&mut gpus,&mut Vec::new()); gpus
    }
    #[test]
    fn amd_without_nvml_reads_temperature_load_hotspot_and_zero_rpm() {
        let g=readings(card("/gpu-amd/0","AMD Radeon RX 7800 XT",json!(55.25)),vec![]).remove(0);
        assert_eq!(g.name,"AMD Radeon RX 7800 XT"); assert_eq!(g.temp,Some(55.25));
        assert_eq!(g.hotspot,Some(72.5)); assert_eq!(g.load,Some(42.5));
        assert_eq!(g.fan_rpm,Some(0.0)); assert_eq!(g.fan_pct,Some(0.0));
        assert_eq!(g.source,"AMD · LibreHardwareMonitor");
    }
    #[test]
    fn mixed_cards_keep_nvidia_primary_readings_and_separate_amd_adapters() {
        let root=json!({"Children":[card("/gpu-amd/0","Radeon",json!(50)),card("/gpu-amd/1","Radeon",json!(60)),card("/gpu-nvidia/0","GeForce",json!(99))]});
        let g=readings(root,vec![GpuData{name:"GeForce".into(),temp:Some(45.0),source:"NVML",..Default::default()}]);
        assert_eq!(g.len(),3); assert_eq!(g[0].temp,Some(45.0));assert_eq!(g[0].hotspot,Some(72.5));
        assert_eq!(g[1].temp,Some(50.0)); assert_eq!(g[2].temp,Some(60.0));
    }
    #[test]
    fn absent_and_invalid_readings_stay_unavailable_not_zero_or_memory_temp() {
        for value in [Value::Null,json!("NaN"),json!(-1),json!(999)] {
            let mut root=card("/gpu-amd/0","Radeon",value);
            root["Children"][0]["Children"][0]["Value"]=json!("65 °C"); // stale formatted value must not override RawValue
            let g=readings(root,vec![]);assert_eq!(g[0].temp,None);
        }
        assert_eq!(sensor_value(&json!("52,25 °C")),Some(52.25));
        assert_eq!(sensor_value(&json!("N/A")),None);
        assert_eq!(sensor_value(&json!(true)),None);
        assert!(readings(json!({"Children":[]}),vec![]).is_empty());
    }
}
