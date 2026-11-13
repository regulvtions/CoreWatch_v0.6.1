#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod desktop;
mod rgb;
mod sensors;
mod fans;
mod fan_store;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use eframe::egui::{
    self, pos2, vec2, Color32, FontId, Frame, Margin, RichText, Rounding, Sense, Shape, Stroke,
    TextStyle, Ui,
};
use tokio::sync::mpsc::UnboundedSender;

use rgb::{Effect, RgbCmd, RgbSettings, RgbStatus};
use sensors::Snapshot;
use fans::{Command as FanCmd, FanStatus, Mode as FanMode, Profile as FanProfile, Source as FanSource};
use std::sync::mpsc::Sender as FanSender;

const HISTORY: usize = 120; // samples kept per graph (~2 min at 1 Hz)
const GRAPH_MIN: f32 = 20.0;
const GRAPH_MAX: f32 = 100.0;

// Palette
const BG: Color32 = Color32::from_rgb(9, 10, 13);
const CARD: Color32 = Color32::from_rgb(18, 20, 25);
const BORDER: Color32 = Color32::from_rgb(36, 39, 47);
const TRACK: Color32 = Color32::from_rgb(29, 32, 39);
const TEXT: Color32 = Color32::from_rgb(232, 236, 244);
const DIM: Color32 = Color32::from_rgb(133, 142, 158);
const ACCENT: Color32 = Color32::from_rgb(77, 145, 240);
const GOOD: Color32 = Color32::from_rgb(74, 222, 128);
const BAD: Color32 = Color32::from_rgb(248, 90, 80);

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Monitor,
    Fans,
    Rgb,
    Options,
}

#[derive(Clone, Copy)]
enum Tile {
    Cpu,
    Gpu(usize),
    NoGpu,
}

struct RgbUi {
    tx: UnboundedSender<RgbCmd>,
    status: Arc<Mutex<RgbStatus>>,
    settings: RgbSettings,
    last_sent: Option<RgbSettings>,
    applied: Option<RgbSettings>,
    applied_gpu: bool,
    react_to_gpu: bool,
    devices_initialized: bool,
}

struct FanUi {
    initial_profile_loaded: bool,
    tx: FanSender<FanCmd>,
    status: Arc<Mutex<FanStatus>>,
    profile: FanProfile,
    source: FanSource,
    manual: f32,
    custom: [(f32, f32); 4],
}

struct App {
    desktop: desktop::Desktop,
    preferences: desktop::Preferences,
    startup_enabled: bool,
    options_message: String,
    force_quit: bool,
    snap: Arc<Mutex<Snapshot>>,
    logo: egui::TextureHandle,
    hist: HashMap<String, VecDeque<f32>>,
    fan_max: HashMap<String, f32>,
    last_seq: u64,
    tab: Tab,
    rgb: RgbUi,
    fans: FanUi,
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("CoreWatch")
            .with_decorations(false)
            .with_icon(egui::IconData { rgba: include_bytes!("../assets/corewatch.rgba").to_vec(), width: 256, height: 256 })
            .with_inner_size([1160.0, 800.0])
            .with_min_inner_size([900.0, 620.0]),
        ..Default::default()
    };
    eframe::run_native(
        "CoreWatch",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);

        let snap = Arc::new(Mutex::new(Snapshot::default()));
        sensors::spawn(snap.clone(), cc.egui_ctx.clone());

        let (tx, status) = rgb::spawn(cc.egui_ctx.clone());
        let _ = tx.send(RgbCmd::Connect);
        let (fan_tx, fan_status) = fans::spawn(snap.clone(), cc.egui_ctx.clone());

        let logo = cc.egui_ctx.load_texture("corewatch-logo",
            egui::ColorImage::from_rgba_unmultiplied([256,256], include_bytes!("../assets/corewatch.rgba")),
            egui::TextureOptions::LINEAR);
        let (preferences, mut options_message) = desktop::Preferences::load();
        let startup_enabled = desktop::startup_enabled().unwrap_or_else(|e| { options_message = e; false });
        Self {
            desktop: desktop::Desktop::new(cc), preferences, startup_enabled, options_message, force_quit: false,
            logo,
            snap,
            hist: HashMap::new(),
            fan_max: HashMap::new(),
            last_seq: 0,
            tab: Tab::Monitor,
            fans: FanUi {
                initial_profile_loaded: false,
                tx: fan_tx, status: fan_status, profile: FanProfile::Balanced,
                source: FanSource::Cpu, manual: 55.0, custom: FanProfile::Custom.points(),
            },
            rgb: RgbUi {
                tx,
                status,
                settings: RgbSettings::default(),
                last_sent: None,
                applied: None,
                applied_gpu: false,
                react_to_gpu: false,
                devices_initialized: false,
            },
        }
    }

    fn ingest(&mut self, s: &Snapshot) {
        if s.seq == self.last_seq {
            return;
        }
        self.last_seq = s.seq;
        push_hist(&mut self.hist, "cpu", s.cpu.temp);
        for (i, g) in s.gpus.iter().enumerate() {
            push_hist(&mut self.hist, &format!("gpu{i}"), g.temp);
        }
        for f in &s.fans {
            let m = self.fan_max.entry(f.label.clone()).or_insert(0.0);
            *m = m.max(f.value);
        }
    }

    /// Draft controls never touch hardware until Apply is clicked. Temperature
    /// effects continue to follow sensors using the last applied settings.
    fn sync_rgb(&mut self, snap: &Snapshot, status: &RgbStatus) {
        if !status.connected {
            self.rgb.devices_initialized = false;
            self.rgb.applied = None;
            self.rgb.last_sent = None;
            return;
        }
        if !self.rgb.devices_initialized {
            self.rgb.settings.devices = status.devices.iter().map(|d| d.index).collect();
            self.rgb.devices_initialized = true;
        }
        let Some(mut cur) = self.rgb.applied.clone() else { return };
        if cur.effect == Effect::Temperature {
            let t = if self.rgb.applied_gpu {
                snap.gpus.iter().filter_map(|g| g.temp).reduce(f32::max)
            } else { snap.cpu.temp };
            cur.temp = t.unwrap_or(30.0).round() as i32;
            if self.rgb.last_sent.as_ref() != Some(&cur) {
                let _ = self.rgb.tx.send(RgbCmd::FollowTemperature(cur.temp));
                self.rgb.last_sent = Some(cur);
            }
        }
    }

}

impl eframe::App for App {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = self.fans.tx.send(FanCmd::Shutdown);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) { self.draw(ctx); }
}

impl App {
    fn draw(&mut self, ctx: &egui::Context) {
        let snap = self.snap.lock().map(|g| g.clone()).unwrap_or_default();
        let rgb_status = self.rgb.status.lock().map(|g| g.clone()).unwrap_or_default();
        let fan_status = self.fans.status.lock().map(|g| g.clone()).unwrap_or_default();
        if !self.fans.initial_profile_loaded && fan_status.online && !fan_status.fans.is_empty() {
            self.fans.initial_profile_loaded = true;
            if let Some(fan) = fan_status.fans.iter().find(|f| !matches!(f.mode,FanMode::Automatic)) {
                match &fan.mode {
                    FanMode::Curve {profile,source,points} => {
                        self.fans.profile=*profile;self.fans.source=*source;self.fans.custom=*points;
                    },
                    FanMode::Manual(duty)=>self.fans.manual=*duty,
                    FanMode::Automatic=>{},
                }
            }
        }
        self.ingest(&snap);
        self.sync_rgb(&snap, &rgb_status);

        self.force_quit |= self.desktop.quit_requested();
        if ctx.input(|i| i.viewport().close_requested()) && self.hide_on_close() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        if window_header(ctx, &self.logo) {
            ctx.send_viewport_cmd(if self.hide_on_close() { egui::ViewportCommand::Visible(false) } else { egui::ViewportCommand::Close });
        }

        egui::TopBottomPanel::bottom("status")
            .frame(
                Frame::none()
                    .fill(BG)
                    .stroke(Stroke::new(1.0_f32, BORDER))
                    .inner_margin(Margin::symmetric(24.0, 8.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let amd_online = snap.gpus.iter().any(|g| g.source.starts_with("AMD"));
                    status_dot(ui, snap.nvml_online || amd_online, if amd_online && snap.nvml_online { "AMD + NVIDIA" } else if amd_online { "AMD Radeon" } else { "GPU sensors" });
                    ui.add_space(10.0);
                    status_dot(ui, snap.lhm_online, "LibreHardwareMonitor");
                    ui.add_space(10.0);
                    status_dot(ui, rgb_status.connected, "OpenRGB");
                    ui.add_space(10.0);
                    status_dot(ui, fan_status.online, "Fan control API");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (rect, response) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::drag());
                        let response = response.on_hover_cursor(egui::CursorIcon::ResizeNwSe).on_hover_text("Resize window");
                        ui.painter().line_segment([rect.left_bottom(), rect.right_top()], Stroke::new(1.0_f32, DIM));
                        if response.drag_started() { ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(egui::ResizeDirection::SouthEast)); }
                    });
                });
            });

        egui::SidePanel::left("navigation")
            .exact_width(190.0).resizable(false)
            .frame(Frame::none().fill(Color32::from_rgb(9, 10, 13))
                .inner_margin(Margin::same(18.0)))
            .show(ctx, |ui| {
                ui.add_space(10.0);
                section_title(ui, "YOUR SYSTEM");
                ui.add_space(8.0);
                for (tab, title, subtitle) in [
                    (Tab::Monitor, "Overview", "Live system telemetry"),
                    (Tab::Fans, "Cooling", "Fan profiles & curves"),
                    (Tab::Rgb, "Lighting", "Your setup, your color"),
                    (Tab::Options, "Options", "Make CoreWatch yours"),
                ] {
                    let selected = self.tab == tab;
                    let response = nav_button(ui, tab, title, selected);
                    if response.clicked() { self.tab = tab; }
                    ui.label(RichText::new(subtitle).size(11.0).color(DIM));
                    ui.add_space(8.0);
                }
                ui.add_space(24.0);
                ui.separator();
                ui.label(RichText::new("LOCAL MONITORING").size(10.0).color(DIM));
                ui.label(RichText::new("1-second sensor polling").size(11.0).color(DIM));
                ui.label(RichText::new("v0.6.1").size(11.0).color(DIM));
            });

        if matches!(self.tab, Tab::Fans | Tab::Rgb) {
            egui::TopBottomPanel::bottom("page_actions")
                .frame(Frame::none().fill(Color32::from_rgb(14, 16, 20))
                    .inner_margin(Margin::symmetric(24.0, 14.0)))
                .show(ctx, |ui| {
                    if self.tab == Tab::Rgb { self.lighting_actions(ui, &rgb_status); }
                    else {
                        ui.horizontal_wrapped(|ui| {
                            if primary_button(ui, "Apply curve to all fans", fan_status.online && !fan_status.fans.is_empty()).clicked() {
                                let points = if self.fans.profile == FanProfile::Custom { self.fans.custom } else { self.fans.profile.points() };
                                let mode = FanMode::Curve { profile: self.fans.profile, source: self.fans.source, points };
                                let ids = fan_status.fans.iter().map(|f| f.id.clone()).collect();
                                let _ = self.fans.tx.send(FanCmd::SetMany(ids, mode));
                            }
                            ui.label(RichText::new(format!("{} curve · {} channels", self.fans.profile.name(), fan_status.fans.len())).size(12.0).color(DIM));
                        });
                        ui.label(RichText::new("Applies to the discovered fan channels below. Pump-named channels are excluded.").size(11.0).color(DIM));
                    }
                });
        }

        egui::CentralPanel::default()
            .frame(Frame::none().fill(BG).inner_margin(Margin::symmetric(24.0, 8.0)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt(match self.tab { Tab::Monitor => "monitor", Tab::Fans => "fans", Tab::Rgb => "rgb", Tab::Options => "options" })
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let (title, subtitle) = match self.tab {
                            Tab::Monitor => ("System overview", "Your hardware, at a glance."),
                            Tab::Fans => ("Cooling control", "Find the balance between airflow and acoustics."),
                            Tab::Rgb => ("Lighting studio", "Make your setup feel like yours."),
                            Tab::Options => ("Options", "A few small settings. A smoother everyday experience."),
                        };
                        ui.add_space(10.0);
                        ui.heading(RichText::new(title).size(28.0).strong().color(TEXT));
                        ui.label(RichText::new(subtitle).color(DIM));
                        ui.add_space(18.0);
                        match self.tab {
                            Tab::Monitor => self.monitor_tab(ui, &snap),
                            Tab::Fans => self.fans_tab(ui, &snap, &fan_status),
                            Tab::Rgb => self.rgb_tab(ui, &rgb_status),
                            Tab::Options => self.options_tab(ui),
                        }
                    });
            });
    }
}

// ---------------------------------------------------------------------------
// Monitor tab
// ---------------------------------------------------------------------------

impl App {
    fn monitor_tab(&self, ui: &mut Ui, snap: &Snapshot) {
        if snap.cpu.temp.is_none() {
            notice(
                ui,
                "CPU temperature isn't available yet",
                "Start LibreHardwareMonitor as Administrator and enable its local Remote Web Server \
                 on port 8085. CoreWatch reads CPU temperatures from http://127.0.0.1:8085/data.json.",
            );
            ui.add_space(4.0);
        }

        let mut tiles = vec![Tile::Cpu];
        if snap.gpus.is_empty() {
            tiles.push(Tile::NoGpu);
        } else {
            tiles.extend((0..snap.gpus.len()).map(Tile::Gpu));
        }

        for chunk in tiles.chunks(2) {
            ui.columns(2, |cols| {
                for (i, tile) in chunk.iter().enumerate() {
                    let ui = &mut cols[i];
                    match *tile {
                        Tile::Cpu => {
                            let stats = vec![("Load", fmt_opt(snap.cpu.load, "%"))];
                            temp_tile(
                                ui,
                                "CPU",
                                &snap.cpu.name,
                                snap.cpu.temp,
                                self.hist.get("cpu"),
                                &stats,
                                snap.cpu.source,
                            );
                        }
                        Tile::Gpu(n) => {
                            let g = &snap.gpus[n];
                            let fan = match (g.fan_pct, g.fan_rpm) {
                                (Some(p), _) => format!("{p:.0}%"),
                                (None, Some(r)) => format!("{r:.0} rpm"),
                                _ => "—".to_string(),
                            };
                            let stats = vec![
                                ("Load", fmt_opt(g.load, "%")),
                                ("Fan", fan),
                                ("Hot spot", fmt_opt(g.hotspot, "°")),
                            ];
                            temp_tile(
                                ui,
                                &format!("GPU {}", n + 1),
                                &g.name,
                                g.temp,
                                self.hist.get(&format!("gpu{n}")),
                                &stats,
                                g.source,
                            );
                        }
                        Tile::NoGpu => {
                            temp_tile(ui, "GPU", "No GPU sensors found", None, None, &[], "AMD: enable LibreHardwareMonitor web server");
                        }
                    }
                }
            });
            ui.add_space(10.0);
        }

        self.fans_card(ui, snap);
    }

    fn fans_card(&self, ui: &mut Ui, snap: &Snapshot) {
        let mut rows: Vec<(String, f32, String)> = Vec::new();
        for f in &snap.fans {
            let max = self.fan_max.get(&f.label).copied().unwrap_or(0.0).max(2000.0);
            rows.push((f.label.clone(), f.value / max, format!("{:.0} RPM", f.value)));
        }
        for (i, g) in snap.gpus.iter().enumerate() {
            let label = format!("GPU {} fan", i + 1);
            if let Some(p) = g.fan_pct {
                rows.push((label, p / 100.0, format!("{p:.0}%")));
            } else if let Some(r) = g.fan_rpm {
                rows.push((label, r / 3000.0, format!("{r:.0} RPM")));
            }
        }

        card(ui, |ui| {
            section_title(ui, "Fans");
            ui.add_space(6.0);
            if rows.is_empty() {
                ui.label(RichText::new("No fan sensors found.").color(DIM));
                if !snap.lhm_online {
                    ui.label(
                        RichText::new("Motherboard and AIO fans are read through LibreHardwareMonitor.")
                            .size(12.5)
                            .color(DIM),
                    );
                }
            } else {
                let bar_w = (ui.available_width() - 330.0).max(70.0);
                egui::Grid::new("fans_grid")
                    .num_columns(3)
                    .spacing([18.0, 12.0])
                    .show(ui, |ui| {
                        for (name, frac, text) in &rows {
                            ui.label(RichText::new(name).color(TEXT));
                            bar(ui, *frac, ACCENT, bar_w);
                            let c = if *frac <= 0.0 { DIM } else { TEXT };
                            ui.label(RichText::new(text).color(c).monospace());
                            ui.end_row();
                        }
                    });
            }
        });
    }
}

fn temp_tile(
    ui: &mut Ui,
    title: &str,
    name: &str,
    temp: Option<f32>,
    hist: Option<&VecDeque<f32>>,
    stats: &[(&str, String)],
    source: &str,
) {
    card(ui, |ui| {
        ui.set_min_height(310.0);
        section_title(ui, title);
        ui.label(RichText::new(name).size(12.5).color(DIM));
        ui.add_space(4.0);

        match temp {
            Some(t) => ui.label(
                RichText::new(format!("{t:.0}°C"))
                    .size(54.0)
                    .strong()
                    .color(temp_color(t)),
            ),
            None => ui.label(RichText::new("—").size(54.0).strong().color(DIM)),
        };

        let color = temp.map(temp_color).unwrap_or(DIM);
        if let Some(h) = hist {
            sparkline(ui, h, color, 100.0);
        } else {
            sparkline(ui, &VecDeque::new(), color, 100.0);
        }

        ui.horizontal(|ui| {
            ui.label(RichText::new("120 samples / ~2 min").size(10.0).color(DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new("20–100 °C").size(10.0).color(DIM));
            });
        });
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            for (k, v) in stats {
                ui.vertical(|ui| {
                    ui.label(RichText::new(*k).size(11.5).color(DIM));
                    ui.label(RichText::new(v).size(15.0).color(TEXT));
                });
                ui.add_space(18.0);
            }
        });

        if !source.is_empty() {
            ui.add_space(4.0);
            ui.label(RichText::new(format!("via {source}")).size(11.0).color(DIM));
        }
    });
}

// ---------------------------------------------------------------------------
// RGB tab
// ---------------------------------------------------------------------------

const SWATCHES: [[u8; 3]; 9] = [
    [255, 255, 255],
    [255, 0, 0],
    [255, 96, 0],
    [255, 220, 0],
    [0, 255, 0],
    [0, 220, 255],
    [0, 70, 255],
    [140, 0, 255],
    [255, 0, 160],
];

impl App {
    fn lighting_actions(&mut self, ui: &mut Ui, status: &RgbStatus) {
        let tx = self.rgb.tx.clone();
        let s = &mut self.rgb.settings;
        ui.horizontal_wrapped(|ui| {
            let enabled = status.connected && !s.devices.is_empty();
            if primary_button(ui, "Apply lighting", enabled).clicked() {
                let mut applied = s.clone();
                applied.temp = self.snap.lock().ok().and_then(|snap| {
                    if self.rgb.react_to_gpu { snap.gpus.iter().filter_map(|g| g.temp).reduce(f32::max) }
                    else { snap.cpu.temp }
                }).unwrap_or(30.0).round() as i32;
                self.rgb.applied_gpu = self.rgb.react_to_gpu;
                self.rgb.applied = Some(applied.clone());
                self.rgb.last_sent = None;
                let _ = tx.send(RgbCmd::Apply(applied));
            }
            let pending = self.rgb.applied.as_ref().map_or(true, |applied| {
                let mut draft = s.clone();
                draft.temp = applied.temp;
                draft != *applied || self.rgb.applied_gpu != self.rgb.react_to_gpu
            });
            ui.label(RichText::new(if !enabled { "Select at least one device" }
                else if pending { "Changes ready to apply" } else { "No unapplied changes" })
                .size(12.0).color(DIM));
        });
        if !status.apply_message.is_empty() {
            ui.label(RichText::new(&status.apply_message).color(if status.apply_failed { BAD } else { DIM }));
        }
    }

    fn rgb_tab(&mut self, ui: &mut Ui, status: &RgbStatus) {
        let tx = self.rgb.tx.clone();

        // Connection card
        card(ui, |ui| {
            section_title(ui, "OpenRGB connection");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                dot(ui, if status.connected { GOOD } else { BAD });
                ui.add(egui::Label::new(RichText::new(&status.message).color(TEXT)).wrap());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if status.connected {
                        if ui.button("Disconnect").clicked() {
                            let _ = tx.send(RgbCmd::Disconnect);
                        }
                        if ui.button("Rescan").clicked() {
                            self.rgb.devices_initialized = false;
                            self.rgb.applied = None;
                            let _ = tx.send(RgbCmd::Connect);

                        }
                    } else if ui.button("Connect").clicked() {
                        let _ = tx.send(RgbCmd::Connect);

                    }
                });
            });
            if !status.connected {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "CoreWatch connects to the OpenRGB SDK at 127.0.0.1:6742. If OpenRGB is installed \
                         as a Windows service, its server may already be running. Otherwise start its SDK server.",
                    )
                    .size(12.5)
                    .color(DIM),
                );
            }
        });

        if !status.connected {
            return;
        }
        ui.add_space(14.0);

        let s = &mut self.rgb.settings;


        if ui.available_width() >= 850.0 {
            ui.columns(2, |cols| {
                lighting_effects(&mut cols[0], s, &mut self.rgb.react_to_gpu);
                lighting_devices(&mut cols[1], s, status);
            });
        } else {
            lighting_effects(ui, s, &mut self.rgb.react_to_gpu);
            lighting_devices(ui, s, status);
        }

    }
}


fn lighting_effects(ui: &mut Ui, s: &mut RgbSettings, react_to_gpu: &mut bool) {
        // Effect card
        card(ui, |ui| {
            section_title(ui, "Choose your look");
            let c = s.color;
            let (preview, _) = ui.allocate_exact_size(vec2(ui.available_width(), 12.0), Sense::hover());
            ui.painter().rect_filled(preview, 6.0, Color32::from_rgb(
                (c[0] as f32 * s.brightness) as u8,
                (c[1] as f32 * s.brightness) as u8,
                (c[2] as f32 * s.brightness) as u8));
            ui.label(RichText::new(format!("Selected #{:02X}{:02X}{:02X} · brightness preview", s.color[0], s.color[1], s.color[2])).size(11.0).color(DIM));
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                for e in Effect::ALL {
                    if ui
                        .selectable_label(s.effect == e, RichText::new(e.label()).size(14.5))
                        .clicked()
                    {
                        s.effect = e;
                    }
                }
            });
            ui.add_space(10.0);

            if s.effect.uses_color() {
                ui.horizontal_wrapped(|ui| {
                    for c in SWATCHES {
                        if swatch(ui, c, s.color == c).clicked() {
                            s.color = c;
                        }
                    }
                    ui.add_space(8.0);
                    ui.color_edit_button_srgb(&mut s.color);
                });
                ui.add_space(8.0);
            }

            if s.effect == Effect::Temperature {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("React to").color(DIM));
                    ui.selectable_value(&mut *react_to_gpu, false, "CPU");
                    ui.selectable_value(&mut *react_to_gpu, true, "GPU");
                });
                ui.label(
                    RichText::new("Blue at 30 °C, fading through green and yellow to red at 90 °C.")
                        .size(12.5)
                        .color(DIM),
                );
                ui.add_space(8.0);
            }

            ui.spacing_mut().slider_width = (ui.available_width() - 165.0).clamp(100.0, 230.0);
            egui::Grid::new("rgb_sliders").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
                ui.label(RichText::new("Brightness").color(DIM));
                ui.add(
                    egui::Slider::new(&mut s.brightness, 0.0..=1.0)
                        .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                );
                ui.end_row();

                if s.effect.uses_speed() {
                    ui.label(RichText::new("Speed").color(DIM));
                    ui.add(egui::Slider::new(&mut s.speed, 0.2..=3.0).suffix("×"));
                    ui.end_row();
                }
            });
        });
        ui.add_space(14.0);


}

fn lighting_devices(ui: &mut Ui, s: &mut RgbSettings, status: &RgbStatus) {
        // Devices card
        card(ui, |ui| {
            ui.horizontal(|ui| {
                section_title(ui, "Select devices");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("None").clicked() {
                        s.devices.clear();
                    }
                    if ui.small_button("All").clicked() {
                        s.devices = status.devices.iter().map(|d| d.index).collect();
                    }
                });
            });
            ui.add_space(6.0);
            if status.devices.is_empty() {
                ui.label(RichText::new("OpenRGB didn't report any controllable devices.").color(DIM));
            }
            for d in &status.devices {
                let mut on = s.devices.contains(&d.index);
                let label = format!("{}   ·   {} LEDs", d.name, d.leds);
                if ui.checkbox(&mut on, RichText::new(label).color(TEXT)).changed() {
                    if on {
                        s.devices.push(d.index);
                        s.devices.sort_unstable();
                    } else {
                        s.devices.retain(|i| *i != d.index);
                    }
                }
            }
        });
}

// ---------------------------------------------------------------------------
// Fan control tab (requires LHM local HTTP server; opt-in per channel)
// ---------------------------------------------------------------------------
impl App {
    fn fans_tab(&mut self, ui: &mut Ui, snap: &Snapshot, status: &FanStatus) {
        ui.label(RichText::new("Applied fan settings are saved and restored when CoreWatch starts.").size(12.0).color(DIM));
        ui.add_space(8.0);
        card(ui, |ui| {
            section_title(ui, "Fan control connection");
            ui.label(RichText::new(&status.message).color(if status.online { GOOD } else { DIM }));
            ui.horizontal_wrapped(|ui| {
                if ui.button("Rescan controls").clicked() { let _ = self.fans.tx.send(FanCmd::Refresh); }
                if ui.button("Restore all to firmware").clicked() { let _ = self.fans.tx.send(FanCmd::ResetAll); }
            });
            egui::CollapsingHeader::new("Setup & fan-control notes").default_open(!status.online).show(ui, |ui| {
                ui.label(RichText::new("Run LibreHardwareMonitor as Administrator with its local web server on port 8085. Fan controls depend on your hardware. Pump-named channels are excluded.").size(12.0).color(DIM));
                ui.label(RichText::new("CoreWatch attempts to restore overrides on normal exit. After a crash or lost connection, verify fan speeds in LibreHardwareMonitor or BIOS.").size(12.0).color(DIM));
            });
        });
        ui.add_space(6.0);
        card(ui, |ui| {
            section_title(ui, "Fan profile");
            ui.label(RichText::new("Preview your curve, then apply it to a channel below.").color(DIM));
            ui.horizontal_wrapped(|ui| {
                ui.label("Curve profile:");
                for p in FanProfile::ALL {
                    ui.selectable_value(&mut self.fans.profile, p, p.name());
                }
            });
            ui.horizontal(|ui| {
                ui.label("Curve temperature:");
                ui.selectable_value(&mut self.fans.source, FanSource::Cpu, "CPU");
                ui.selectable_value(&mut self.fans.source, FanSource::GpuMax, "GPU (hottest)");
            });
            let temp = if self.fans.source == FanSource::Cpu { snap.cpu.temp } else { snap.gpus.iter().filter_map(|g| g.temp).reduce(f32::max) };
            ui.label(RichText::new(format!("Current source: {}", temp.map(|v| format!("{v:.0}°C")).unwrap_or_else(|| "unavailable (100% fail-safe)".into()))).color(DIM));

            if self.fans.profile == FanProfile::Custom {
                ui.label("Custom curve: adjust output at these temperature breakpoints");
                for i in 0..4 {
                    ui.horizontal(|ui| {
                        ui.label(format!("{:.0}°C", self.fans.custom[i].0));
                        ui.add(egui::Slider::new(&mut self.fans.custom[i].1, 35.0..=100.0).suffix("%"));
                    });
                }
            } else {
                let points = self.fans.profile.points();
                ui.label(RichText::new(format!("Preset curve: {}", points.iter().map(|(t,d)| format!("{t:.0}°C: {d:.0}%")).collect::<Vec<_>>().join("  |  "))).size(12.0).color(DIM));
            }
            let points = if self.fans.profile == FanProfile::Custom { self.fans.custom } else { self.fans.profile.points() };
            curve_preview(ui, &points, temp);
            ui.label(RichText::new("Editing this preview does not change running fans until you apply it.").size(12.0).color(DIM));
            egui::CollapsingHeader::new("Manual override settings").show(ui, |ui| {
                ui.add(egui::Slider::new(&mut self.fans.manual, 35.0..=100.0).suffix("%"));
                ui.label(RichText::new("Use Manual override on an individual channel below.").size(12.0).color(DIM));
            });
        });
        ui.add_space(6.0);
        if status.fans.is_empty() {
            notice(ui, "No controllable fan channels discovered", "Check that the LibreHardwareMonitor HTTP server is enabled and its sensor tree contains Control sensors. Not all motherboards and fan headers expose writable controls.");
        }
        for device in &status.fans {
            card(ui, |ui| {
                section_title(ui, &device.name);
                ui.label(RichText::new(&device.id).size(10.0).color(DIM));
                let mode = match &device.mode {
                    FanMode::Automatic => "Firmware / auto".to_owned(),
                    FanMode::Manual(v) => format!("Manual: {v:.0}%"),
                    FanMode::Curve { profile, source, .. } => format!("{} curve · {}", profile.name(), if *source == FanSource::Cpu { "CPU" } else { "GPU" }),
                };
                ui.label(format!("Current mode: {mode}  |  Last command: {}", device.last_duty.map(|v| format!("{v:.0}%")).unwrap_or_else(|| "none".into())));
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Manual override").clicked() {
                        let _ = self.fans.tx.send(FanCmd::SetMode(device.id.clone(), FanMode::Manual(self.fans.manual)));
                    }
                    if ui.button("Use selected curve").clicked() {
                        let points = if self.fans.profile == FanProfile::Custom { self.fans.custom } else { self.fans.profile.points() };
                        let _ = self.fans.tx.send(FanCmd::SetMode(device.id.clone(), FanMode::Curve { profile: self.fans.profile, source: self.fans.source, points }));
                    }
                    if ui.button("Restore auto").clicked() {
                        let _ = self.fans.tx.send(FanCmd::SetMode(device.id.clone(), FanMode::Automatic));
                    }
                });
            });
            ui.add_space(4.0);
        }
    }
}

// ---------------------------------------------------------------------------
// Widgets & helpers
// ---------------------------------------------------------------------------

fn primary_button(ui: &mut Ui, label: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(enabled, egui::Button::new(RichText::new(label).strong().color(Color32::WHITE))
        .fill(Color32::from_rgb(2, 62, 138)).rounding(24.0).min_size(vec2(160.0, 38.0)))
}

fn window_header(ctx: &egui::Context, logo: &egui::TextureHandle) -> bool {
    let mut close = false;
    egui::TopBottomPanel::top("window_header").exact_height(54.0)
        .frame(Frame::none().fill(Color32::BLACK).inner_margin(Margin::symmetric(18.0, 9.0)))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.image((logo.id(), vec2(40.0, 32.0)));
                ui.label(RichText::new("CoreWatch").size(19.0).strong().color(TEXT));
                ui.add_space(12.0);
                ui.label(RichText::new("SYSTEM CONTROL").size(10.0).color(DIM));
                let drag_width = (ui.available_width() - 136.0).max(10.0);
                let (_, drag) = ui.allocate_exact_size(vec2(drag_width, 32.0), Sense::click_and_drag());
                if drag.double_clicked() {
                    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                } else if drag.drag_started() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                for (kind, name) in [(0, "Minimize"), (1, "Maximize / restore"), (2, "Close")] {
                    let (rect, response) = ui.allocate_exact_size(vec2(34.0, 32.0), Sense::click());
                    let response = response.on_hover_text(name);
                    let p = ui.painter();
                    if response.hovered() { p.rect_filled(rect, 6.0, if kind == 2 { Color32::from_rgb(153, 35, 48) } else { TRACK }); }
                    let c = rect.center();
                    let stroke = Stroke::new(1.3_f32, TEXT);
                    if kind == 0 { p.line_segment([c + vec2(-5.0, 0.0), c + vec2(5.0, 0.0)], stroke); }
                    else if kind == 1 { p.rect_stroke(egui::Rect::from_center_size(c, vec2(10.0, 10.0)), 1.0, stroke); }
                    else {
                        p.line_segment([c + vec2(-4.0,-4.0), c + vec2(4.0,4.0)], stroke);
                        p.line_segment([c + vec2(-4.0,4.0), c + vec2(4.0,-4.0)], stroke);
                    }
                    if response.clicked() {
                        match kind {
                            0 => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                            1 => ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!ctx.input(|i| i.viewport().maximized.unwrap_or(false)))),
                            _ => close = true,
                        }
                    }
                }
            });
        });
    close
}

fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::none()
        .fill(CARD)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .rounding(Rounding::same(14.0))
        .inner_margin(Margin::same(18.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

fn notice(ui: &mut Ui, title: &str, body: &str) {
    Frame::none()
        .fill(Color32::from_rgb(38, 31, 16))
        .stroke(Stroke::new(1.0_f32, Color32::from_rgb(92, 72, 28)))
        .rounding(Rounding::same(12.0))
        .inner_margin(Margin::same(14.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(title)
                    .strong()
                    .color(Color32::from_rgb(250, 204, 21)),
            );
            ui.label(RichText::new(body).size(13.0).color(Color32::from_rgb(214, 196, 150)));
        });
    ui.add_space(8.0);
}

fn section_title(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text.to_uppercase()).size(12.0).strong().color(DIM));
}

fn sparkline(ui: &mut Ui, data: &VecDeque<f32>, color: Color32, height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 8.0, Color32::from_rgb(17, 20, 25));
    for fraction in [0.25, 0.5, 0.75] {
        let y = rect.top() + rect.height() * fraction;
        p.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], Stroke::new(1.0_f32, BORDER));
    }
    if data.len() < 2 {
        p.text(rect.center(), egui::Align2::CENTER_CENTER, "Collecting temperature history",
            FontId::proportional(11.0), DIM);
        return;
    }

    let pad = 6.0;
    let pts: Vec<egui::Pos2> = data
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let x = rect.right() - (data.len() - 1 - i) as f32 / (HISTORY - 1) as f32 * rect.width();
            let k = ((*v - GRAPH_MIN) / (GRAPH_MAX - GRAPH_MIN)).clamp(0.0, 1.0);
            pos2(x, rect.bottom() - pad - k * (rect.height() - pad * 2.0))
        })
        .collect();

    // Soft gradient fill under the line
    let top = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 70);
    let bottom = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 0);
    let mut mesh = egui::Mesh::default();
    for (i, pt) in pts.iter().enumerate() {
        mesh.colored_vertex(*pt, top);
        mesh.colored_vertex(pos2(pt.x, rect.bottom()), bottom);
        if i > 0 {
            let b = (i * 2) as u32;
            mesh.add_triangle(b - 2, b - 1, b);
            mesh.add_triangle(b - 1, b + 1, b);
        }
    }
    p.add(Shape::mesh(mesh));
    p.add(Shape::line(pts, Stroke::new(2.0_f32, color)));
}

fn curve_preview(ui: &mut Ui, points: &[(f32, f32); 4], current: Option<f32>) {
    ui.add_space(8.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 170.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 10.0, BG);
    let plot = rect.shrink2(vec2(38.0, 24.0));
    let xy = |t: f32, duty: f32| pos2(
        plot.left() + ((t - 20.0) / 80.0).clamp(0.0, 1.0) * plot.width(),
        plot.bottom() - duty.clamp(0.0, 100.0) / 100.0 * plot.height());
    for duty in [0.0, 50.0, 100.0] {
        let y = xy(20.0, duty).y;
        p.line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], Stroke::new(1.0_f32, BORDER));
        p.text(pos2(plot.left() - 5.0, y), egui::Align2::RIGHT_CENTER,
            format!("{duty:.0}%"), FontId::proportional(10.0), DIM);
    }
    for t in [20.0, 40.0, 60.0, 80.0, 100.0] {
        p.text(pos2(xy(t, 0.0).x, plot.bottom() + 12.0), egui::Align2::CENTER_CENTER,
            format!("{t:.0}°"), FontId::proportional(10.0), DIM);
    }
    let mut line = vec![xy(20.0, points[0].1)];
    line.extend(points.iter().map(|&(t, d)| xy(t, d)));
    line.push(xy(100.0, points[3].1));
    p.add(Shape::line(line, Stroke::new(2.5_f32, ACCENT)));
    for &(t, duty) in points { p.circle_filled(xy(t, duty), 4.0, ACCENT); }
    if let Some(t) = current {
        let x = xy(t, 0.0).x;
        p.line_segment([pos2(x, plot.top()), pos2(x, plot.bottom())], Stroke::new(1.0_f32, GOOD));
    }
    ui.label(RichText::new("Temperature / fan duty   ·   green marker: current source").size(11.0).color(DIM));
}

fn bar(ui: &mut Ui, frac: f32, color: Color32, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 8.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 4.0, TRACK);
    let mut fill = rect;
    fill.set_width(rect.width() * frac.clamp(0.0, 1.0));
    if fill.width() > 0.5 {
        p.rect_filled(fill, 4.0, color);
    }
}

fn swatch(ui: &mut Ui, c: [u8; 3], selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
    let col = Color32::from_rgb(c[0], c[1], c[2]);
    let p = ui.painter();
    p.circle_filled(rect.center(), 11.0, col);
    if selected {
        p.circle_stroke(rect.center(), 13.5, Stroke::new(2.0_f32, TEXT));
    } else if resp.hovered() {
        p.circle_stroke(rect.center(), 13.5, Stroke::new(1.5_f32, DIM));
    }
    resp
}

fn dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.5, color);
}

fn status_dot(ui: &mut Ui, on: bool, label: &str) {
    dot(ui, if on { GOOD } else { Color32::from_rgb(78, 84, 96) });
    ui.label(RichText::new(label).size(12.0).color(DIM));
}

fn fmt_opt(v: Option<f32>, unit: &str) -> String {
    v.map(|v| format!("{v:.0}{unit}")).unwrap_or_else(|| "—".to_string())
}

fn push_hist(h: &mut HashMap<String, VecDeque<f32>>, key: &str, v: Option<f32>) {
    if let Some(v) = v {
        let q = h.entry(key.to_string()).or_default();
        if q.len() >= HISTORY {
            q.pop_front();
        }
        q.push_back(v);
    }
}

fn temp_color(t: f32) -> Color32 {
    const STOPS: [(f32, [u8; 3]); 4] = [
        (35.0, [96, 165, 250]),
        (55.0, [74, 222, 128]),
        (72.0, [250, 204, 21]),
        (88.0, [248, 90, 80]),
    ];
    let c = |a: [u8; 3]| Color32::from_rgb(a[0], a[1], a[2]);
    if t <= STOPS[0].0 {
        return c(STOPS[0].1);
    }
    for w in STOPS.windows(2) {
        if t <= w[1].0 {
            let k = (t - w[0].0) / (w[1].0 - w[0].0);
            let l = |i: usize| (w[0].1[i] as f32 + (w[1].1[i] as f32 - w[0].1[i] as f32) * k) as u8;
            return Color32::from_rgb(l(0), l(1), l(2));
        }
    }
    c(STOPS[STOPS.len() - 1].1)
}

fn apply_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.extreme_bg_color = Color32::from_rgb(10, 12, 15);
    v.faint_bg_color = CARD;
    v.selection.bg_fill = Color32::from_rgb(2, 62, 138);
    v.selection.stroke = Stroke::new(1.0_f32, Color32::WHITE);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    v.widgets.inactive.bg_fill = TRACK;
    v.widgets.inactive.weak_bg_fill = TRACK;
    v.widgets.hovered.bg_fill = Color32::from_rgb(40, 45, 56);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(40, 45, 56);
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
        w.rounding = Rounding::same(8.0);
    }
    ctx.set_visuals(v);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = vec2(10.0, 10.0);
    style.spacing.button_padding = vec2(16.0, 9.0);
    style.spacing.slider_width = 230.0;
    style.spacing.interact_size.y = 32.0;
    style.text_styles.insert(TextStyle::Body, FontId::proportional(14.5));
    style.text_styles.insert(TextStyle::Button, FontId::proportional(14.5));
    ctx.set_style(style);
}

#[cfg(test)]
mod ui_tests;

impl App {
    fn hide_on_close(&self) -> bool {
        desktop::should_hide(self.preferences.close_to_tray, self.desktop.tray_available, self.force_quit)
    }
    fn options_tab(&mut self, ui: &mut Ui) {
        card(ui, |ui| {
            section_title(ui, "WINDOW BEHAVIOR");
            let old = self.preferences.close_to_tray;
            if ui.add_enabled(self.desktop.tray_available, egui::Checkbox::new(&mut self.preferences.close_to_tray, "Minimize to tray when I close the window")).changed() {
                self.options_message = match self.preferences.save() {
                    Ok(()) => "Options saved.".into(),
                    Err(e) => { self.preferences.close_to_tray = old; e }
                };
            }
            ui.label(RichText::new("Keep monitoring, cooling and lighting running in the background.").color(DIM));
            ui.add_space(10.0);
            ui.label(RichText::new("Double-click the tray icon to return, or right-click it for Open CoreWatch and Quit CoreWatch.").size(12.0).color(DIM));
        });
        ui.add_space(16.0);
        card(ui, |ui| {
            section_title(ui, "WINDOWS STARTUP");
            let old = self.startup_enabled;
            if ui.add_enabled(cfg!(windows), egui::Checkbox::new(&mut self.startup_enabled, "Launch when I sign in to Windows")).changed() {
                self.options_message = match desktop::set_startup(self.startup_enabled) {
                    Ok(()) => "Options saved.".into(),
                    Err(e) => { self.startup_enabled = old; format!("Could not update startup: {e}") }
                };
            }
            ui.label(RichText::new("Start CoreWatch automatically for your Windows account.").color(DIM));
            ui.add_space(10.0);
            ui.label(RichText::new("Keep CoreWatch in a permanent folder. If you move it, turn this option off and on again to update its location.").size(12.0).color(DIM));
        });
        ui.add_space(16.0);
        if !self.desktop.error.is_empty() { ui.label(RichText::new(&self.desktop.error).color(DIM)); }
        if !self.options_message.is_empty() { ui.label(&self.options_message); }
        ui.add_space(10.0);
        if ui.button("Quit CoreWatch").clicked() {
            self.force_quit = true;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ui.label(RichText::new("Fully exits CoreWatch, even when close to tray is enabled.").size(12.0).color(DIM));
    }
}

fn nav_button(ui: &mut Ui, tab: Tab, title: &str, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 42.0), Sense::click());
    let p = ui.painter();
    if selected || response.hovered() {
        p.rect_filled(rect, 8.0, if selected { Color32::from_rgb(22,40,68) } else { TRACK });
    }
    let color = if selected { ACCENT } else { TEXT };
    let c = pos2(rect.left()+20.0, rect.center().y);
    let stroke = Stroke::new(1.6_f32, color);
    let line = |a: (f32,f32), b: (f32,f32)| { p.line_segment([c+vec2(a.0,a.1), c+vec2(b.0,b.1)],stroke); };
    match tab {
        Tab::Monitor => {
            p.rect_stroke(egui::Rect::from_center_size(c-vec2(0.0,2.0),vec2(19.0,13.0)),2.5,stroke);
            line((0.0,4.5),(0.0,9.0)); line((-5.0,9.0),(5.0,9.0));
            for (a,b) in [((-6.0,0.0),(-3.0,0.0)),((-3.0,0.0),(-1.0,-4.0)),((-1.0,-4.0),(2.0,1.0)),((2.0,1.0),(5.0,-2.0))] {line(a,b);}
        }
        Tab::Fans => {
            p.circle_stroke(c,2.2,stroke);
            for i in 0..3 {
                let angle = i as f32 * std::f32::consts::TAU/3.0;
                let points:Vec<_>=(0..=18).map(|j| {
                    let t=j as f32*std::f32::consts::TAU/18.0;
                    let x=5.8+3.5*t.cos(); let y=2.3*t.sin();
                    c+vec2(x*angle.cos()-y*angle.sin(),x*angle.sin()+y*angle.cos())
                }).collect();
                p.add(Shape::line(points,stroke));
            }
        }
        Tab::Rgb => {
            p.circle_stroke(c-vec2(0.0,2.0),5.5,stroke);
            line((-3.0,4.0),(-3.0,7.0)); line((3.0,4.0),(3.0,7.0));
            line((-3.0,7.0),(3.0,7.0)); line((-1.5,10.0),(1.5,10.0));
            line((-9.0,-2.0),(-11.0,-2.0)); line((9.0,-2.0),(11.0,-2.0)); line((0.0,-10.0),(0.0,-12.0));
        }
        Tab::Options => {
            for (y,x) in [(-7.0,-3.0),(0.0,4.0),(7.0,-1.0)] {
                line((-9.0,y),(x-2.5,y)); line((x+2.5,y),(9.0,y));
                p.circle_stroke(c+vec2(x,y),2.5,stroke);
            }
        }
    }
    p.text(pos2(rect.left()+42.0,rect.center().y),egui::Align2::LEFT_CENTER,title,FontId::proportional(14.0),color);
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, title));
    response
}
