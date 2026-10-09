# X11 capture geometry repair, 2026-10-09

Linux's existing X11 compatibility command used the requested encoded width/height as the input capture rectangle. A 1920×1080 request therefore did not establish scaling of a larger source. The new command reads the selected DISPLAY screen's current root geometry, captures that whole existing root and then scales it into the requested output bounds. For a 3840×2160 root and 1920×1080 request, the command now uses `-video_size 3840x2160 -i :0+0,0 -vf scale=w=1920:h=1080`.

## Source and input preservation

The production source remains the existing X11 root window at origin 0,0. There is no new physical-display selector, portal capture or backend fallback. A physical primary monitor is not inferred from the root metadata; complete native Wayland desktop content remains unverified. The generic source catalog and its capability/readiness gaps are unchanged.

The inspected `LinuxUinputInjector` maps absolute input over the desktop coordinate range. Switching this repair to a nonzero-offset primary-monitor rectangle without a bound input transform would risk incorrect clicks. The implementation therefore retains the root source/coordinate scope. Nonzero capture offsets are covered by a fixed command-planning fixture but are not enabled by a new product source-selection path. The proposed Wayland adapter must separately resolve that input/source binding.

`host/src/linux_capture_geometry.rs` uses x11rb GetGeometry, with no pixel read or display-mode changes. Missing geometry and sources beyond the existing 8192-per-axis / 33,554,432-pixel host capture budget fail before spawning FFmpeg. The planner keeps source dimensions and offset separate from output dimensions, fits the full source into the requested bounds without enlargement, and rounds final output dimensions to even pixels for the existing 4:2:0 encoder. Unknown/invalid source size never falls back to the requested size.

The X11 command logs actual source geometry, capture origin, planned encoded dimensions and requested bounds. The general start log now calls the request “output bounds.” The codec, packet transport, renderer, native viewer buffers, input injector, source IDs, KMS fallback, Windows capture path and persistent configuration are unchanged. Correctly capturing a larger source can cost more than the old cropped rectangle; no physical performance claim or deployment is made.

## Fixed regression evidence

The final command was:

```text
CARGO_TARGET_DIR=/Users/jinliang/rust-target cargo test -p host --lib linux_capture_geometry::tests --offline --locked -- --include-ignored --nocapture
```

**7 passed, 0 failed**, including an explicitly enabled test that sends generated raw RGB pixels to the local real FFmpeg scale filter. It never uses a screen capture device or opens a window. Each process has a ten-second bound. The fixture checks actual output byte dimensions and interior colors in all four source quadrants, so a crop retaining only the top-left quadrant would fail.

| Source pixels | Requested bounds | Actual raw output verified |
| --- | --- | --- |
| 3840×2160 | 1920×1080 | 1920×1080 |
| 2560×1600 | 1280×720 | 1152×720 |
| 1080×1920 | 1920×1080 | 606×1080 |
| 1280×720 | 1920×1080 | 1280×720 |
| 1919×1079 | 1920×1080 | 1918×1078 |

The other six tests cover full-source input size, nonzero offset `+1920,120`, aspect fitting, no enlargement, preserved root origin and invalid/over-budget geometry. They do not connect to an X server. The offline FFmpeg fixture is ignored by the default suite because it requires FFmpeg; the explicit final command ran it successfully. Local FFmpeg was version 8.0.1.

The metadata reader and command planner compiled on macOS through cfg(test), using the exact pinned x11rb 0.13.2. Linux's guarded integration was not freshly built or physically tested; native desktop content, real clicks, performance, HEVC/SPS dimensions on HO5 and actual GPUI presentation remain unverified. The only final build warning was the pre-existing unused PermissionsExt import in remote_core.

## Dependencies and delivery boundary

The host adds an exact Linux-only dependency reference to x11rb 0.13.2 with default features disabled, plus the same dev dependency for the metadata/planner tests. This crate version already exists in the product lockfile; no package/version was added or updated. Cargo.lock changed only the host's dependency entry. The locked offline metadata check passed. No GTK, egui, new GUI framework, xrandr CLI dependency or native library link was introduced by this repair.

All 105 unrelated dirty paths retain their original bytes/status. The existing target was reused; no production rebuild, remote command, real-window experiment, installation, service change, permission request, NAS publication or push was performed in this phase. Prior signed packages and acceptance receipts remain separate and untouched.

Raw final test output and implementation boundaries are saved in [x11-capture-geometry.json](evidence/x11-capture-geometry.json). The next implementation remains a proposal in [LINUX-WAYLAND-CAPTURE-ADAPTER.md](../../plans/LINUX-WAYLAND-CAPTURE-ADAPTER.md).
