<div align="center">
  <img src="./corewatch.png" alt="CoreWatch Logo" width="180">
  <h1>CoreWatch</h1>
  <p><strong>Your hardware. Your cooling. Your color.</strong></p>
  <p>A native Rust desktop app for Windows, bringing hardware monitoring, fan curves, and RGB control into one clean workspace.</p>
  <p><a href="#features">Features</a> · <a href="#getting-started">Getting started</a> · <a href="#build-from-source">Build from source</a> · <a href="#compatibility">Compatibility</a></p>
</div>

---

## Features

### Overview

See CPU and GPU temperatures, utilization, and available fan readings at a glance. Live graphs show recent temperature history, with sensor updates every second.

- NVIDIA GPU monitoring through NVML.
- AMD Radeon monitoring through LibreHardwareMonitor.
- GPU hotspot temperature and fan speed where supported.
- Multiple GPU support, including mixed AMD and NVIDIA systems.

### Cooling

Choose a Quiet, Balanced, Performance, or Custom fan curve. Use CPU or GPU temperature as the source, apply a curve across available channels, or adjust individual fans.

- Custom temperature-to-speed curves.
- Manual control for supported channels.
- An option to return discovered controls to automatic operation.

### Lighting

Control compatible devices through OpenRGB with Static, Breathing, Rainbow, Temperature, and Off effects.

- Color, brightness, and effect-speed controls.
- Individual device selection.
- Explicit **Apply lighting** action, so you can finish choosing your settings before sending them.

### Everyday use

A dark interface with blue accents, clear navigation, and a custom window header keeps the focus on your system. Optional close-to-tray and Windows sign-in launch settings are available in **Options**.

## Getting started

If you already have a compiled `corewatch.exe`, place it in a permanent folder and launch it. You do not need Rust, VS Code, or the source files to run that executable.

Enable the companion tools for the features you want:

| Feature | Requirement |
| --- | --- |
| NVIDIA GPU readings | Compatible NVIDIA GPU and installed NVIDIA driver |
| CPU temperatures and AMD GPU readings | LibreHardwareMonitor with its local web server enabled on port `8085` |
| Fan control | Writable fan controls exposed by LibreHardwareMonitor |
| RGB lighting | OpenRGB with its SDK server running on port `6742` |
| Tray and sign-in options | Windows; enable these settings inside CoreWatch |

### Set up monitoring and cooling

1. Start [LibreHardwareMonitor](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor). Run it as administrator if required for hardware access.
2. Enable its web server on port `8085` and keep it running.
3. Open CoreWatch. Sensor readings appear automatically when available.

CoreWatch connects locally to `http://127.0.0.1:8085/data.json`. Keep the control server restricted to your own computer.

### Set up lighting

1. Start [OpenRGB](https://openrgb.org/) and confirm it detects your devices.
2. Enable its SDK server at `127.0.0.1:6742`.
3. In CoreWatch, open **Lighting**, connect if needed, and select your devices.
4. Choose your settings and click **Apply lighting**.

Apply again after changing settings, reconnecting, or rescanning devices. OpenRGB's SDK port is an application connection, not a page to open in a web browser.

### Use the tray and startup options

Both options are off by default.

- **Close to tray:** Closing the window keeps CoreWatch running. Double-click its tray icon to return, or use **Open CoreWatch** from the tray menu.
- **Quit:** Choose **Quit CoreWatch** from the tray menu or Options page to exit completely.
- **Launch at sign-in:** Enable this after moving the executable to its permanent location. If you move it later, switch the option off and on again to update the path.

CoreWatch's startup setting does not start OpenRGB or LibreHardwareMonitor; configure those applications separately if needed.

## Build from source

On Windows, install Rust with the MSVC toolchain and Microsoft C++ Build Tools, including the Windows SDK. Open a terminal in the project folder containing `Cargo.toml`, then run:

```powershell
cargo build --release
```

Launch the compiled application:

```powershell
.\target\release\corewatch.exe
```

Or build and launch in one step:

```powershell
cargo run --release
```

Keep `assets` and `vendor` alongside `src` when building. The Windows executable includes the CoreWatch icon.

## Compatibility

CoreWatch is designed for Windows 11. Available readings and controls depend on your hardware, drivers, and what LibreHardwareMonitor and OpenRGB expose.

- Missing readings appear as a dash. A reported fan speed of zero can indicate a stopped fan.
- AMD monitoring is implemented, but physical Radeon hardware validation is still pending.
- An RPM reading does not necessarily mean a fan supports software control.
- Pump-named controls are excluded from batch fan control. Check the listed channels before applying a curve.
- Fan curves use a minimum duty of 35%; a missing curve temperature requests 100% duty.
- Normal exit requests fan-control cleanup, but restoration is best-effort. Check cooling after a crash or forced exit, and avoid competing fan-control applications.
- Lighting devices may retain their last color when CoreWatch exits.

## Project structure

| Path | Purpose |
| --- | --- |
| `src/main.rs` | Interface, navigation, graphs, and user actions |
| `src/sensors.rs` | CPU and GPU monitoring |
| `src/fans.rs` | Fan profiles and control worker |
| `src/rgb.rs` | OpenRGB connection and lighting effects |
| `src/desktop.rs` | Tray integration and Windows startup preferences |
| `assets/` | Application logo and icon files |
| `vendor/` | OpenRGB client with compatibility fixes |
| `build.rs` | Windows icon embedding |

## Acknowledgments

Built with Rust and egui/eframe, with hardware integration through LibreHardwareMonitor, NVIDIA NVML, and OpenRGB. Third-party components retain their respective licenses; the vendored OpenRGB client's license is included under `vendor/`.
