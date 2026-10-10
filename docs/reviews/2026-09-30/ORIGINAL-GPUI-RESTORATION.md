# Original GPUI restoration: verified feasibility and actual progress

## Decision

Restore the established GPUI/yororen_ui desktop design across macOS, Linux and Windows. Do not continue the basic egui UI as the intended product presentation, and do not switch to Flutter merely because the original project gated its GUI dependencies to macOS. The same locked GPUI CE 0.3.3 and yororen_ui 0.2.0 have now compiled and opened real native windows on all three desktop OS families.

Flutter remains the user-approved contingency only if actual full-product portability, maintainability or native-video requirements prove unworkable. GPUI desktop feasibility is not evidence of Android support. Android still has its existing Compose/MediaCodec layer and shared Rust protocol. Flutter's official platform docs include Android and the three desktop OS families, but changing UI framework does not eliminate native texture and decoder integration. The latest exact stable patch was not successfully obtained from the attempted release manifest, so this report does not invent one. No Flutter SDK installation or rewrite was performed.

## What was actually tested

The native-window probe reuses the existing design_system and the original idle-stage and status-capsule visual functions. It does not start a mesh runtime, subscribe to any host, capture a screen, save a screenshot, or inject input. These functions were then extracted to original_design.rs and used by both the original Mac product source and the probe, rather than copied into a different product design.

| Device | Native rendering result | GPU evidence |
|---|---|---|
| Mac Studio | Original shared components rendered in a real native window; timed exit and receipt confirmed | The GPUI macOS gpu_specs API returned None; this is not claimed as a measured GPU device or performance benchmark. Source backend is the existing Metal path. |
| HO5 | Same original components compiled, opened and rendered; desktop environment includes Wayland | GPU reported AMD Radeon 890M Graphics (RADV STRIX1), software_emulated=false. |
| Windows cube | Same components compiled, opened and rendered; bounded native probe exited | GPU reported AMD Radeon(TM) 8060S Graphics, software_emulated=false. UU service PID/state unchanged; no capture or mesh runtime started. |

Linux initially lacked the development linker names libxkbcommon.so and libxkbcommon-x11.so, not the runtime libraries. A private staging link directory points to the already installed .so.0 libraries; LIBRARY_PATH is set only for this build. No system package, graphics driver, service or OS configuration was modified. This is packaging/dependency handling, not evidence the framework cannot support Linux.

Cocoa terminates its application without returning to main. The first Mac probe therefore rendered and exited but did not write the after-run JSON. The probe was corrected to persist observed render evidence before requesting quit. The final native receipts are the ones used here.

## Code changes now present

1. app/Cargo.toml exposes the existing GPUI and yororen_ui dependencies for the three desktop targets, retaining Cargo.lock. A gpui-restoration feature gates the restoration-only adapter/probe work; the production default entry has not been switched.
2. app/src/original_design.rs contains the original idle-stage and capsule component bodies, with only visibility/owner genericization changes. app/src/ui.rs uses those same functions. Original visual colors and component structure are retained.
3. app/src/desktop/model.rs now has a generic SessionState<Texture, Pan>. The existing egui Session alias preserves its behavior, while OriginalGuiSession retains native decoded frames and uses GPUI coordinates. Networking, file identities, source revisions, pause/error and input safety remain one implementation, not a rollback to the older legacy session flow.
4. app/src/desktop/original_presenter.rs adopts a current-source decoded frame by ownership/reference and provides the original macOS CVPixelBuffer -> GPUI surface boundary. It deliberately does not convert frames to RGBA. A separate exact-frame paint acknowledgement prevents an old, paused or unconfirmed source from enabling input. This adapter is compiled/tested but not yet wired to the full production restored window.
5. app/examples/original_gpui_probe.rs validates the actual old design components on a native window. It is not a replacement GUI and its receipt explicitly says full_product_restored=false and video_tested=false.

## Performance finding and required correction

The current simplified desktop presentation on macOS calls client/src/desktop_frame.rs, allocates a full RGBA frame and iterates over NV12 pixels on the CPU before uploading to egui. That is not equivalent to the original native CVPixelBuffer surface path. Restoring only theme colors while retaining that path does not satisfy the user's performance requirement.

The original macOS surface boundary is retained in the new adapter; this does not yet remove the conversion from the installed app because the full default presentation has not switched. GPUI's locked native SurfaceSource/CVPixelBuffer path is macOS-only, so the Linux/Windows renderer still needs a proper video-buffer/texture import path. GPU acceleration of a static widget does not prove hardware decoding, zero-copy video, 4K/60fps capability, latency or long-duration stability.

Performance acceptance before final rollout must cover real decoded-to-displayed frame latency, GPU/CPU usage, copies per frame, bounded buffering and memory after repeated sessions. UI work must not run a CPU pixel conversion or copy/serialize full frames through widget state or a language bridge. Native platform adapters are permitted; duplicated protocol/business logic is not.

## Final automated regression

With the shared Session/native presentation boundary added:
- Mac App library: 87 passed; the 1 explicitly ignored protocol-input test was then run separately and passed.
- Linux App library: 72 passed; the 1 explicitly ignored protocol-input test was then run separately and passed.
- Windows App library: 72 passed; the 1 explicitly ignored protocol-input test was then run separately and passed.
- All-target cargo check with gpui-restoration: passed on the three platforms.
These are repeated platform tests, not independent feature totals. The input regression uses a local recorder and does not claim native GPUI pointer/keyboard end-to-end acceptance. No new full UI screenshot/visual approval, streaming or performance benchmark is claimed.

## Not yet complete

The full original control island, drawer, source tabs, transfers, diagnostics, pop-outs and their current secure model interactions are not yet all reconnected to one restored cross-platform product view. Linux/Windows native video presentation and full actual visual/interaction acceptance remain work. No original GUI was reinstalled as a production replacement in this turn, no service/identity/permissions changed, and no NAS release was published. Installed alpha.7 continues to run; its visual regression has not yet been corrected on the devices.

Do not report this as 'restoration complete'. The confirmed decision is that the original framework is viable on the user's three desktop platforms, and the implementation has started by preserving original components and reusing current safe state rather than rewriting the entire GUI stack again.

## Evidence locations and public references

Main source/backup/change manifests: /Users/jinliang/Workspace/.rp-original-gui-20260930.
Mac native and regression receipts: /Users/jinliang/workspace/.rp-original-gui-20260930.
Linux native and regression receipts: /var/home/liang/workspace/.rp-original-gui-20260930.
Windows native and regression receipts: C:/Users/Liang/Workspace/.rp-original-gui-20260930.

Public primary references: https://gpui-ce.github.io/ ; https://docs.rs/gpui-ce/0.3.3/gpui/ ; https://docs.rs/yororen_ui/0.2.0/yororen_ui/ ; https://docs.flutter.dev/reference/supported-platforms ; https://api.flutter.dev/flutter/widgets/Texture-class.html .

Source remains uncommitted. Existing input/relay/signed-install sources were checked unchanged against the initial snapshot, except the intentional renderer-generic Session refactor. No benchmark or publication result is inferred from these checks.
