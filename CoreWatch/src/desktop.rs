//! Optional Windows integration. Creating the tray does not enable autostart.
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences { pub close_to_tray: bool }

fn preferences_path() -> Result<PathBuf, String> {
    let root = std::env::var_os("LOCALAPPDATA").ok_or("Windows settings folder is unavailable")?;
    Ok(PathBuf::from(root).join("CoreWatch").join("preferences.json"))
}
impl Preferences {
    pub fn load() -> (Self, String) {
        if !cfg!(windows) { return (Self::default(), String::new()); }
        let result = preferences_path().and_then(|p| match std::fs::read(p) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("Could not read saved options: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("Could not read saved options: {e}")),
        });
        match result { Ok(p) => (p,String::new()), Err(e) => (Self::default(),e) }
    }
    pub fn save(&self) -> Result<(),String> { self.save_to(&preferences_path()?) }
    fn save_to(&self, path: &Path) -> Result<(),String> {
        if let Some(parent)=path.parent() { std::fs::create_dir_all(parent).map_err(|e|e.to_string())?; }
        let bytes=serde_json::to_vec_pretty(self).map_err(|e|e.to_string())?;
        std::fs::write(path,bytes).map_err(|e|format!("Could not save options: {e}"))
    }
}

pub struct Desktop {
    #[cfg(windows)]
    _tray: Option<tray_icon::TrayIcon>,
    quit: Arc<AtomicBool>,
    pub tray_available: bool,
    pub error: String,
}
impl Desktop {
    pub fn disabled() -> Self { Self {
        #[cfg(windows)] _tray:None,
        quit:Arc::new(AtomicBool::new(false)), tray_available:false, error:String::new(),
    } }
    pub fn quit_requested(&self) -> bool { self.quit.load(Ordering::SeqCst) }
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = &cc.egui_ctx;
        let mut desktop=Self::disabled();
        #[cfg(windows)] {
            use tray_icon::{TrayIconBuilder, TrayIconEvent, MouseButton, Icon};
            use tray_icon::menu::{Menu,MenuItem,MenuEvent};
            let result=(|| -> Result<tray_icon::TrayIcon,String> {
                let window = NativeWindow::from_creation_context(cc)?;
                let menu=Menu::new();
                let show=MenuItem::new("Open CoreWatch",true,None);
                let quit=MenuItem::new("Quit CoreWatch",true,None);
                menu.append(&show).map_err(|e|e.to_string())?;
                menu.append(&quit).map_err(|e|e.to_string())?;
                let show_id=show.id().clone(); let quit_id=quit.id().clone();
                let icon=Icon::from_rgba(include_bytes!("../assets/corewatch.rgba").to_vec(),256,256).map_err(|e|e.to_string())?;
                let tray=TrayIconBuilder::new().with_icon(icon).with_tooltip("CoreWatch")
                    .with_menu(Box::new(menu)).build().map_err(|e|e.to_string())?;
                let menu_ctx=ctx.clone(); let quitting=desktop.quit.clone();
                MenuEvent::set_event_handler(Some(move |event:MenuEvent| {
                    if event.id==show_id { window.show(&menu_ctx); }
                    else if event.id==quit_id {
                        quitting.store(true,Ordering::SeqCst);
                        // Wake the actual HWND before requesting normal eframe shutdown.
                        // Queued egui commands alone cannot wake a hidden Win32 window.
                        window.close(&menu_ctx);
                    }
                }));
                let tray_ctx=ctx.clone();
                TrayIconEvent::set_event_handler(Some(move |event:TrayIconEvent| {
                    if matches!(event,TrayIconEvent::DoubleClick {button:MouseButton::Left,..}) { window.show(&tray_ctx); }
                }));
                Ok(tray)
            })();
            match result { Ok(tray)=>{desktop._tray=Some(tray);desktop.tray_available=true;},Err(e)=>desktop.error=format!("Tray unavailable: {e}") }
        }
        #[cfg(not(windows))] { let _=ctx; desktop.error="Tray and sign-in options are available on Windows.".into(); }
        desktop
    }
}

// Callbacks run on the Win32 UI thread. Capture the actual eframe HWND, not
// a title lookup (which can target the tray's hidden window or another instance).
#[cfg(windows)]
#[derive(Clone, Copy)]
struct NativeWindow(isize);

#[cfg(windows)]
impl NativeWindow {
    fn from_creation_context(cc: &eframe::CreationContext<'_>) -> Result<Self, String> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        match cc.window_handle().map_err(|e| e.to_string())?.as_raw() {
            RawWindowHandle::Win32(handle) => Ok(Self(handle.hwnd.get())),
            _ => Err("Windows window handle is unavailable".into()),
        }
    }

    fn show(self, ctx: &egui::Context) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, ShowWindow, SetForegroundWindow, SW_RESTORE, SW_SHOW};
        let hwnd = self.0 as windows_sys::Win32::Foundation::HWND;
        // SAFETY: this is the live main window obtained from CreationContext;
        // tray callbacks execute on its owning Windows event-loop thread.
        unsafe {
            ShowWindow(hwnd, if IsIconic(hwnd) != 0 { SW_RESTORE } else { SW_SHOW });
            SetForegroundWindow(hwnd);
        }
        // ShowWindow wakes the hidden HWND but bypasses winit's cached flags.
        // Synchronize those flags before the next Close or Minimize action.
        sync_restored_window(ctx);
    }

    fn close(self, ctx: &egui::Context) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
        self.show(ctx);
        // Use the normal close lifecycle so on_exit releases fan control.
        // The quit flag is set BEFORE posting, so close-to-tray cannot cancel it.
        unsafe { PostMessageW(self.0 as windows_sys::Win32::Foundation::HWND, WM_CLOSE, 0, 0); }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        ctx.request_repaint();
    }
}

#[cfg(any(windows, test))]
fn sync_restored_window(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    ctx.request_repaint();
}

pub fn should_hide(close_to_tray:bool,tray_available:bool,quit:bool)->bool {
    close_to_tray && tray_available && !quit
}

#[cfg(windows)]
const RUN_KEY: &str=r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const RUN_VALUE: &str="CoreWatch";

#[cfg(any(windows, test))]
fn startup_command(executable: &Path) -> Result<String,String> {
    let path=executable.to_str().ok_or("Executable path cannot be represented as text")?;
    if path.contains('"') {return Err("Executable path contains a quotation mark".into());}
    let command=format!("\"{path}\"");
    if command.encode_utf16().count()>260 {return Err("Move CoreWatch to a shorter folder path to enable sign-in launch".into());}
    Ok(command)
}

#[cfg(windows)]
pub fn startup_enabled() -> Result<bool,String> {
    use winreg::{RegKey,enums::*};
    let key=match RegKey::predef(HKEY_CURRENT_USER).open_subkey(RUN_KEY) {
        Ok(k)=>k,Err(e) if e.kind()==std::io::ErrorKind::NotFound=>return Ok(false),Err(e)=>return Err(e.to_string()),
    };
    match key.get_value::<String,_>(RUN_VALUE) {
        Ok(value)=>Ok(!value.is_empty()),
        Err(e) if e.kind()==std::io::ErrorKind::NotFound=>Ok(false),
        Err(e)=>Err(e.to_string()),
    }
}
#[cfg(not(windows))]
pub fn startup_enabled() -> Result<bool,String> {Ok(false)}

#[cfg(windows)]
pub fn set_startup(enabled:bool) -> Result<(),String> {
    use winreg::{RegKey,enums::*};
    let (key,_)=RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN_KEY).map_err(|e|e.to_string())?;
    if enabled {
        let exe=std::env::current_exe().map_err(|e|e.to_string())?;
        key.set_value(RUN_VALUE,&startup_command(&exe)?).map_err(|e|e.to_string())
    } else {
        match key.delete_value(RUN_VALUE) {Ok(())=>Ok(()),Err(e) if e.kind()==std::io::ErrorKind::NotFound=>Ok(()),Err(e)=>Err(e.to_string())}
    }
}
#[cfg(not(windows))]
pub fn set_startup(_enabled:bool)->Result<(),String>{Err("Available on Windows only".into())}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn close_policy_never_hides_without_a_tray_or_on_quit() {
        assert!(should_hide(true,true,false));
        assert!(!should_hide(true,false,false));
        assert!(!should_hide(true,true,true));
        assert!(!should_hide(false,true,false));
    }
    #[test]
    fn restore_synchronizes_visibility_before_minimize_and_focus() {
        let ctx = egui::Context::default();
        let out = ctx.run(egui::RawInput::default(), sync_restored_window);
        let commands = &out.viewport_output[&egui::ViewportId::ROOT].commands;
        assert!(matches!(commands.as_slice(), [egui::ViewportCommand::Visible(true), egui::ViewportCommand::Minimized(false), egui::ViewportCommand::Focus]));
    }
    #[test] fn launch_paths_with_spaces_are_quoted() {
        assert_eq!(startup_command(Path::new(r"C:\My Apps\CoreWatch\corewatch.exe")).unwrap(),r#""C:\My Apps\CoreWatch\corewatch.exe""#);
    }
    #[test] fn settings_default_off_and_round_trip() {
        assert!(!Preferences::default().close_to_tray);
        let root=std::env::temp_dir().join(format!("corewatch-pref-test-{}",std::process::id()));
        let path=root.join("prefs.json");
        Preferences {close_to_tray:true}.save_to(&path).unwrap();
        let loaded:Preferences=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(loaded.close_to_tray);std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    #[test]
    fn tray_actions_wake_hidden_native_window_without_an_egui_frame() {
        // A real HWND with the built-in STATIC class; no CoreWatch instance,
        // registry entry or hardware workers are started by this regression test.
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        unsafe {
            let hwnd = CreateWindowExW(0, class.as_ptr(), class.as_ptr(), WS_OVERLAPPEDWINDOW,
                0, 0, 160, 100, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null());
            assert!(!hwnd.is_null());
            struct Cleanup(windows_sys::Win32::Foundation::HWND);
            impl Drop for Cleanup { fn drop(&mut self) { unsafe { DestroyWindow(self.0); } } }
            let _cleanup = Cleanup(hwnd);
            let window = NativeWindow(hwnd as isize);
            let ctx = egui::Context::default();
            assert_eq!(IsWindowVisible(hwnd), 0);
            window.show(&ctx);
            assert_ne!(IsWindowVisible(hwnd), 0, "Open must work without rendering an egui frame");
            ShowWindow(hwnd, SW_HIDE);
            window.close(&ctx);
            assert_ne!(IsWindowVisible(hwnd), 0, "Quit must wake the native window");
            let mut msg: MSG = std::mem::zeroed();
            assert_ne!(PeekMessageW(&mut msg, hwnd, WM_CLOSE, WM_CLOSE, PM_REMOVE), 0);
        }
    }
}
