use super::*;
use serde_json::json;

fn fixture(ctx: &egui::Context) -> (App, tokio::sync::mpsc::UnboundedReceiver<RgbCmd>, std::sync::mpsc::Receiver<FanCmd>) {
    apply_theme(ctx);
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (fan_tx, fan_rx) = std::sync::mpsc::channel();
    let snap = Snapshot {
        seq: 1,
        cpu: sensors::CpuData { name: "Intel Core i9-9900KF".into(), temp: Some(48.0), load: Some(18.0), source: "LibreHardwareMonitor HTTP" },
        gpus: vec![sensors::GpuData { name: "NVIDIA GeForce RTX 4060".into(), temp: Some(43.0), load: Some(12.0), fan_pct: Some(35.0), hotspot: Some(53.0), source: "NVML", ..Default::default() }],
        fans: vec![sensors::Reading { label: "CPU fan".into(), value: 1080.0 }], nvml_online: true, lhm_online: true,
    };
    let devices = ["Gigabyte GeForce RTX 4060 Gaming OC", "MSI ROG STRIX Z690-E GAMING WIFI", "NZXT Smart Device V2", "PRO 2 LIGHTSPEED"].iter().enumerate().map(|(i, name)| rgb::DeviceInfo { index: i as u32, name: name.to_string(), leds: 16 }).collect();
    let fan_devices = ["CPU fan", "Front intake", "Rear exhaust"].iter().enumerate().map(|(i,n)| fans::FanDevice { id: format!("/fan/{i}"), name: n.to_string(), mode: FanMode::Automatic, last_duty: None }).collect();
    let logo = ctx.load_texture("logo", egui::ColorImage::from_rgba_unmultiplied([256,256], include_bytes!("../assets/corewatch.rgba")), egui::TextureOptions::LINEAR);
    (App {
        desktop: desktop::Desktop::disabled(), preferences: desktop::Preferences::default(), startup_enabled:false, options_message:String::new(), force_quit:false,
        snap: Arc::new(Mutex::new(snap)), logo, hist: HashMap::new(), fan_max: HashMap::new(), last_seq: 0, tab: Tab::Monitor,
        rgb: RgbUi { tx, status: Arc::new(Mutex::new(RgbStatus { connected: true, devices, message: "Connected · 4 devices".into(), ..Default::default() })), settings: RgbSettings::default(), last_sent: None, applied: None, applied_gpu: false, react_to_gpu: false, devices_initialized: false },
        fans: FanUi { initial_profile_loaded:false, tx: fan_tx, status: Arc::new(Mutex::new(FanStatus { online: true, message: "Connected · 3 available fan channels".into(), fans: fan_devices })), profile: FanProfile::Balanced, source: FanSource::Cpu, manual: 55.0, custom: FanProfile::Custom.points() },
    }, rx, fan_rx)
}

#[test]
fn rgb_drafts_do_not_send_until_apply_and_temperature_uses_applied_source() {
    let ctx = egui::Context::default();
    let (mut app, mut rx, _fan_rx) = fixture(&ctx);
    let snap = app.snap.lock().unwrap().clone();
    let status = app.rgb.status.lock().unwrap().clone();
    app.sync_rgb(&snap, &status);
    app.rgb.settings.color = [255,0,0];
    app.sync_rgb(&snap, &status);
    assert!(rx.try_recv().is_err(), "Draft edits must not send hardware commands");
    let mut applied = app.rgb.settings.clone();
    applied.effect = Effect::Temperature;
    app.rgb.applied = Some(applied);
    app.rgb.applied_gpu = false;
    app.rgb.react_to_gpu = true; // unapplied UI edit must not change active source
    app.sync_rgb(&snap, &status);
    assert!(matches!(rx.try_recv(), Ok(RgbCmd::FollowTemperature(48))));
}

#[test]
fn render_all_pages_at_default_and_minimum_size() {
    for (label, tab) in [("overview", Tab::Monitor), ("lighting", Tab::Rgb), ("cooling", Tab::Fans), ("options", Tab::Options)] {
        for (size_name, size) in [("default", vec2(1160.0,800.0)), ("minimum", vec2(900.0,620.0))] {
            let ctx = egui::Context::default();
            let (mut app, _rx, _fan_rx) = fixture(&ctx);
            app.tab = tab;
            app.hist.insert("cpu".into(), (0..120).map(|i| 46.0 + (i as f32 * 0.14).sin() * 4.0).collect());
            app.hist.insert("gpu0".into(), (0..120).map(|i| 42.0 + (i as f32 * 0.12).sin() * 3.0).collect());
            let mut textures = Vec::new();
            for pass in 0..3 {
                let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(pos2(0.0,0.0),size)), ..Default::default() };
                let out = ctx.run(input, |ctx| app.draw(ctx));
                for (id, delta) in &out.textures_delta.set {
                    let image = &delta.image;
                    let pixels: Vec<[u8;4]> = match image {
                        egui::ImageData::Color(c) => c.pixels.iter().map(|c| c.to_array()).collect(),
                        egui::ImageData::Font(f) => f.srgba_pixels(None).map(|c| c.to_array()).collect(),
                    };
                    textures.push(json!({"id": format!("{id:?}"), "size": image.size(), "pos": delta.pos, "pixels": pixels}));
                }
                if pass == 2 {
                    let meshes: Vec<_> = ctx.tessellate(out.shapes, out.pixels_per_point).into_iter().filter_map(|p| {
                        if let egui::epaint::Primitive::Mesh(m) = p.primitive {
                            Some(json!({"texture":format!("{:?}", m.texture_id), "clip":[p.clip_rect.min.x,p.clip_rect.min.y,p.clip_rect.max.x,p.clip_rect.max.y], "indices":m.indices, "vertices":m.vertices.iter().map(|v| json!([v.pos.x,v.pos.y,v.uv.x,v.uv.y,v.color.to_array()])).collect::<Vec<_>>() }))
                        } else {None}
                    }).collect();
                    assert!(!meshes.is_empty());
                    if let Ok(dir) = std::env::var("COREWATCH_UI_CAPTURE") {
                        std::fs::create_dir_all(&dir).unwrap();
                        std::fs::write(format!("{dir}/{label}-{size_name}.json"), serde_json::to_vec(&json!({"size":[size.x,size.y],"textures":textures,"meshes":meshes})).unwrap()).unwrap();
                    }
                }
            }
        }
    }
}

fn click_label(ctx: &egui::Context, app: &mut App, label: &str) {
    fn find(shapes: &[egui::epaint::ClippedShape], label: &str) -> Option<egui::Pos2> {
        shapes.iter().find_map(|s| if let egui::Shape::Text(t) = &s.shape {
            if t.galley.job.text == label { Some(t.pos + t.galley.size() * 0.5) } else { None }
        } else { None })
    }
    let mut pos = None;
    for _ in 0..3 {
        let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(pos2(0.0,0.0),vec2(900.0,620.0))), ..Default::default() }, |ctx| app.draw(ctx));
        pos = find(&out.shapes, label);
    }
    let pos=pos.expect("Apply button must be visible at minimum size");
    assert!(pos.x < 900.0 && pos.y < 620.0);
    for pressed in [true,false] {
        let _ = ctx.run(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(pos2(0.0,0.0),vec2(900.0,620.0))),
            events: vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button:egui::PointerButton::Primary, pressed, modifiers:egui::Modifiers::default() }], ..Default::default()
        }, |ctx| app.draw(ctx));
    }
}

#[test]
fn lighting_apply_click_sends_draft_and_can_retry() {
    let ctx=egui::Context::default();
    let (mut app,mut rx,_fan_rx)=fixture(&ctx);
    app.tab=Tab::Rgb;
    app.rgb.settings.color=[2,62,138];
    for _ in 0..2 {
        click_label(&ctx,&mut app,"Apply lighting");
        match rx.try_recv().expect("Apply must send a command") {
            RgbCmd::Apply(s) => { assert_eq!(s.color,[2,62,138]); assert_eq!(s.devices.len(),4); }
            _ => panic!("Wrong command"),
        }
    }
}

#[test]
fn all_fans_click_sends_one_batch_with_current_curve() {
    let ctx=egui::Context::default();
    let (mut app,_rx,fan_rx)=fixture(&ctx);
    app.tab=Tab::Fans;
    app.fans.profile=FanProfile::Custom;
    app.fans.custom=[(30.,40.),(50.,60.),(70.,80.),(85.,100.)];
    click_label(&ctx,&mut app,"Apply curve to all fans");
    match fan_rx.try_recv().expect("All-fans button must send a batch") {
        FanCmd::SetMany(ids,FanMode::Curve {points,profile,..}) => {
            assert_eq!(ids,vec!["/fan/0","/fan/1","/fan/2"]);
            assert_eq!(points,app.fans.custom);
            assert_eq!(profile,FanProfile::Custom);
        }
        _ => panic!("Wrong fan command"),
    }
}

#[test]
fn native_close_hides_only_with_working_tray_and_explicit_quit_wins() {
    for (tray, enabled, quit, expected_hide) in [(true,true,false,true),(false,true,false,false),(true,false,false,false),(true,true,true,false)] {
        let ctx = egui::Context::default();
        let (mut app, _rx, _fans) = fixture(&ctx);
        app.desktop.tray_available = tray;
        app.preferences.close_to_tray = enabled;
        app.force_quit = quit;
        let mut input = egui::RawInput::default();
        input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().events.push(egui::ViewportEvent::Close);
        let out = ctx.run(input, |ctx| app.draw(ctx));
        let commands = &out.viewport_output[&egui::ViewportId::ROOT].commands;
        assert_eq!(commands.iter().any(|c| matches!(c,egui::ViewportCommand::CancelClose)),expected_hide);
        assert_eq!(commands.iter().any(|c| matches!(c,egui::ViewportCommand::Visible(false))),expected_hide);
    }
}

#[test]
fn restored_curve_populates_editor_without_sending_new_commands() {
    let ctx=egui::Context::default();let (mut app,_rgb,fan_rx)=fixture(&ctx);
    let points=[(30.,35.),(50.,60.),(70.,85.),(90.,100.)];
    app.fans.status.lock().unwrap().fans[0].mode=FanMode::Curve{profile:FanProfile::Custom,source:FanSource::GpuMax,points};
    let _=ctx.run(egui::RawInput::default(),|ctx|app.draw(ctx));
    assert_eq!(app.fans.profile,FanProfile::Custom);assert_eq!(app.fans.source,FanSource::GpuMax);assert_eq!(app.fans.custom,points);
    assert!(fan_rx.try_recv().is_err());
}
