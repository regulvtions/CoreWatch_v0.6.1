# CoreWatch compatibility patches

Upstream: openrgb 0.1.2, https://crates.io/crates/openrgb/0.1.2
Source: https://github.com/nicoulaj/openrgb-rs
License: GPL-2.0; upstream LICENSE is included.

1. Add ModeFlag::RequiresEntireDevice (1 << 10), matching OpenRGB's
   RGBController/RGBControllerInterface.h, and a regression test for flags 1056.
2. Derive Clone for Mode so CoreWatch can send mode-specific color updates without
   consuming its cached mode description. No wire layout changes.

3. Frame reads by their declared length into an isolated cursor, allowing trailing
   extension bytes without desynchronizing the TCP stream. Handle device-list
   notifications, cap response size at 8 MiB, and report actual invalid magic bytes.
   Four framing regressions are included in protocol.rs.

4. Include the four-byte internal size field in update_leds, update_zone_leds,
   and update_mode payload lengths. OpenRGB NetworkServer.cpp validates this
   value against the full packet payload. CoreWatch mock-server tests enforce
   this check for its LED and mode writes and reproduced the prior failure.
