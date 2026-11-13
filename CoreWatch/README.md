# CoreWatch 0.6.1

Hardware monitoring, fan curves, and lighting in a dark desktop interface.

## Install this update

1. Close the old CoreWatch window.
2. Extract this archive into a NEW folder, keeping your old project as a backup.
3. In VS Code open the `CoreWatch` folder containing `Cargo.toml`.
4. Keep the `assets` and `vendor` directories beside `src`.
5. In the terminal run:

```powershell
cargo check
cargo run --release
```

The Windows executable is `target\release\corewatch.exe`. The Windows build embeds
the polished blue logo using the resource compiler from your C++ Build Tools.

## 0.6.1 Repeated tray restore fix

After waking the native window, tray Open and double-click now synchronize the
UI framework's visible and minimized state. This fixes the stale hidden flag
that made X ineffective after restoring and caused Minimize to hide the window.
X follows the close-to-tray preference; Minimize remains a taskbar minimize.
Quit still exits through the normal cleanup path. No diagnostic UI is added.

Development validation: 16 portable tests and Windows-target compilation checks.
Windows desktop behavior still requires confirmation on the Windows PC.

## 0.6.0 AMD Radeon monitoring

AMD Radeon GPUs now have explicit LibreHardwareMonitor integration for core
temperature, hotspot, utilization, fan percentage and RPM where the card exposes
them. NVIDIA continues using NVML. Mixed AMD/NVIDIA and multiple AMD adapters
are handled separately; the status bar reflects the detected GPU sources.
Numeric and formatted sensor values are supported; unavailable readings show a
dash, while a stopped fan correctly reads zero. No debug UI is added.

For AMD readings, keep LibreHardwareMonitor running with its web server enabled
on localhost port 8085. The existing one-second polling loop and HTTP request
are reused; no additional AMD background service or polling worker is added.
This release adds monitoring support; it does not add direct AMD tuning controls.
RGB still uses OpenRGB. Sensor availability depends on hardware and LHM support.

Development validation: 15 automated tests pass and Windows-target checks pass.
Physical AMD hardware has not been validated here. Source mapping follows
LibreHardwareMonitor's AmdGpu sensor definitions:
https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/blob/master/LibreHardwareMonitorLib/Hardware/Gpu/AmdGpu.cs

## 0.5.1 Tray Open / Quit fix

Tray Open and double-click now show the actual Windows window immediately,
rather than queuing visibility commands that wait for a hidden window to render.
Quit wakes that window and posts a normal close request after setting the explicit
quit flag, preserving the shutdown cleanup and bypassing close-to-tray.

Before replacing 0.5.0, end its stuck `corewatch.exe` process in Task Manager.
Build and launch this version, close to tray, then test Open, double-click and Quit.
If you enabled startup and moved folders, toggle startup off and on from this version.

Validation: Linux regression tests and Windows-target compilation are checked.
A Windows-only regression test exercises Open/Quit on a real hidden HWND without
an egui render frame (`cargo test` on Windows). The live tray still needs confirmation
on your PC.

## 0.5.0 Icons, Options and quieter lighting feedback

- Matching vector icons for Overview, Cooling, Lighting and Options.
- Options: minimize to tray on close; launch at Windows sign-in. Both default off.
- Double-click the tray icon to restore the window, or right-click for Open CoreWatch / Quit CoreWatch.
- The Options page also has a Quit button that always exits. If tray creation fails, closing exits normally so the app cannot become inaccessible.
- Tray preferences are saved for your Windows account. Startup changes only CoreWatch's current-user launch entry; administrator access is not required.
- Lighting success now shows only “Lighting applied to N device(s).” Error and mismatch details remain available. The working RGB commands, saturated colors and breathing behavior are preserved.

### Enable startup after building

Run the release executable from the permanent folder where you intend to keep it,
then enable the Windows startup option. If you move the executable, turn the option
off and on again from the new location. Rebuilding at the same path is fine.
This starts CoreWatch at sign-in; OpenRGB and LibreHardwareMonitor still need their
own startup configuration. Close-to-tray keeps monitoring and control running;
choose Quit when you want to stop CoreWatch.

### Validation

12 automated tests pass: RGB protocol/readback and breathing behavior, Apply actions,
settings persistence and defaults, close/quit policy, and rendering all four pages
at default and minimum window sizes. Windows-target `cargo check` passes.
Actual Windows tray restore, Windows sign-in launch, and physical hardware behavior
must be checked on the Windows PC; this build was validated from Linux.

Quick Windows check: enable close-to-tray, click X, restore via double-click, then
try Alt+F4 and restore through the tray menu. Confirm monitoring still updates.
Choose Quit and confirm the process exits. Enable startup from the permanent
release executable and test your next Windows sign-in; disable it to remove the entry.

## 0.4.3 RGB behavior retained

Saturated presets and hue-preserving breathing remain unchanged. Explicit Apply
still checks the server's reported controller state. Successful checks no longer
print raw RGB/mode data. This checks server state rather than measuring physical LEDs.

## 0.4.2 OpenRGB command-length fix

Fixes incorrect internal payload lengths in LED, zone-LED, and mode-update
commands. The old client excluded the four-byte size field itself. Current
OpenRGB server code compares the internal length to the complete payload and
can reject these commands while the TCP write still succeeds.

The simulated server now enforces the same length check. Both lighting tests
failed against 0.4.1 (67 declared bytes versus 71 actual bytes for a mode command)
and pass with this correction. The tests also validate the LED payload length
and chosen color after the mode command succeeds.

Close the old app, build this version, connect, select a device/color, and click
Apply lighting. The sidebar should show v0.4.2. OpenRGB itself does not need to
be reinstalled. Physical lighting still requires a check on the Windows PC.

## 0.4.1 OpenRGB packet fix

Addresses the reported `expected OpenRGB magic value, got "79"` failure.
The old error incorrectly printed the expected byte, not the received byte.

- Read each complete SDK reply using its declared payload length before decoding.
- Keep trailing metadata inside that packet instead of leaving it on the socket.
- Handle device-list notifications between replies, with a bounded notification loop.
- Report actual unexpected header bytes and limit packet size to 8 MiB.
- Disconnect on communication failures instead of reusing a broken stream for every device.

Four framing regressions cover trailing fields, notifications, fragmented packets,
truncated payload isolation, and actual header diagnostics. Both RGB mock-server
paths include extra metadata to exercise consecutive replies. These tests and the
Windows-target compile check pass; physical-device verification is still required.

After updating, close the old CoreWatch process completely. Start this version,
click Connect, then Apply lighting. If it fails, send the new full error message.

## What's new

- Custom black title bar with drag, double-click maximize/restore, minimize and close.
- Bottom-right resize handle; charcoal panels and blue accents.
- Approved polished blue logo in the header, window icon, and Windows executable.
- Lighting controls and device selection sit side by side on wider windows, stacking
  on smaller windows. Each page scrolls independently.
- Fixed bottom action bars keep Apply available at small window sizes.
- Lighting changes are drafts until **Apply lighting** is clicked. Connecting alone
  does not change the lights. Apply always resends, even for unchanged settings.
- RGB mode selection explicitly sets a supported Direct, Custom, or Static mode,
  sets its color source and brightness, and checks the server's active mode index.
- Per-LED hardware receives LED updates; mode-specific hardware receives mode colors.
- Per-device errors are displayed. A timeout drops the connection so a partially read
  protocol response is not reused. Reconnect and Apply to retry.
- **Apply curve to all fans** sends the currently selected profile, source, and
  custom points to every currently discovered controllable fan channel.
- The OpenRGB mode-flag 1056 compatibility patch is retained.

## Lighting

Keep the OpenRGB SDK server running at `127.0.0.1:6742`. With the Windows service
installation this can already be running in the background. This port is a binary
SDK endpoint, not a website.

1. Open Lighting and connect.
2. Select devices and choose an effect, color, brightness, and speed.
3. Click **Apply lighting**. Review the result below the button.
4. After changing a selection or setting, click Apply again.

Applied animated effects keep running while you edit drafts or visit another page.
Temperature lighting follows the last applied CPU/GPU selection. A reconnect or
rescan stops CoreWatch's effect stream; Apply again after discovery completes.
Deselecting a device stops future updates after Apply; it does not restore a previous
hardware effect. Devices may retain the last color after disconnect or app exit.

"Commands sent" means the SDK writes completed, not that the physical LEDs were
verified. If a device still does not change, test its Direct/Static mode in OpenRGB
itself and check whether another lighting application is overriding it.

## Cooling and monitoring

Run LibreHardwareMonitor as Administrator and enable its local web server on port
8085. CoreWatch reads `http://127.0.0.1:8085/data.json`; NVIDIA readings also use NVML.

Choose Quiet, Balanced, Performance, or Custom and the CPU/GPU temperature source.
Use **Apply curve to all fans**, or apply a curve/manual override to individual
channels below. **Restore all to firmware** requests the existing automatic control
behavior. Draft edits do not alter previously applied curves.

Only writable Control sensors exposed by LibreHardwareMonitor are listed. Pump-named
channels are excluded. This name filter cannot identify every possible pump, so
check the discovered channels before using the all-fans action. RPM sensors alone
do not imply control support. Minimum duty is 35%; missing curve temperature requests
100%. Verify actual cooling, and avoid simultaneous fan overrides from multiple apps.

Normal exit requests cleanup, but restoration is best-effort. After a crash, forced
exit, or API loss, verify fan behavior in LibreHardwareMonitor or BIOS. CoreWatch does
not install drivers or modify BIOS settings. Keep the control server restricted to
localhost.

## Validation

- Windows-target `cargo check` passed on the authoring Linux host.
- Application tests cover both lighting protocol paths, repeated Apply, draft
  isolation, the all-fans button payload, and rendering all three pages at 1160x800
  and 900x620. Button tests simulate clicks at the minimum window size.
- Both simulated RGB protocol tests also passed in release mode.
- The vendored client's regression test for OpenRGB flags 1056 passed.
- Layouts were rendered from the actual egui draw output and visually inspected.
  The images in `previews` show the 0.5.0 layout with simulated telemetry, not live PC readings. Windows-only option controls appear disabled in these Linux-rendered samples.
- Windows window-manager behavior, Windows resource embedding, and physical lighting
  and fan operation require verification on a Windows PC. No Windows executable is
  supplied in this source archive.

```powershell
cargo test
cargo test --manifest-path vendor/openrgb-0.1.2/Cargo.toml test_read_entire_device_flag
```

## Source layout

- `src/main.rs`: interface, explicit Apply actions, custom window header.
- `src/rgb.rs`: SDK worker, mode preparation, effects, and mock-server tests.
- `src/fans.rs`: local fan control, batch application, and fail-safe curve behavior.
- `src/sensors.rs`: hardware telemetry; Windows-only ACPI fallback.
- `src/ui_tests.rs`: headless interaction and layout tests.
- `assets`: approved icon formats; `build.rs`: Windows icon resource embedding.
- `vendor/openrgb-0.1.2`: pinned compatibility-patched dependency and upstream license.
