# Fixed GPUI native-video extension: explicit implementation scope

Status: approved by the user: “可以重点是对齐和性能”. The fixed GPUI native-video extension may be implemented within the following scope. Implementation/testing does not itself authorize an unverified complete release.

## Why this is different from an application adapter

The actual locked gpui-ce 0.3.3 dependency has only the macOS CVPixelBuffer variant in SurfaceSource. Its Windows DirectX draw_surfaces function returns Ok without drawing surfaces. Linux's existing video surface shader path is compiled only for macOS/Metal resources. Restored application code on Linux/Windows consequently still shows the explicit native-video-adapter placeholder.

The Blade dependency exposes external memory mechanisms, including DMA handles, but the presence of that API does not prove compatibility with a decoder frame's plane offsets, modifiers, queue ownership or completion fences. This must be proven on HO5 before deployment. It is not an excuse to substitute CPU RGBA conversion or claim hardware performance from a static GPU widget.

The proposed additional scope is a pinned, small native-video extension to the original GPUI renderer rather than replacing its framework, appearance or business model. This carries maintenance responsibility for third-party rendering code and therefore needs an explicit scope decision. No Cargo dependency, graphics backend, system driver or installed product is changed by this proposal.

## Proposed implementation

1. Keep the current original GPUI/yororen design and all application controls. Keep the existing shared Rust device/session/crypto/source-revision/input/files state. Do not revive old legacy connection code.
2. Pin the audited GPUI source revision used by the build. Add narrowly scoped native video-buffer descriptors, plane/format metadata, native-resource ownership and renderer import/composition support for Windows and Linux. Preserve the Mac CVPixelBuffer path. Do not track mutable upstream main or perform an unreviewed whole-toolkit upgrade.
3. Connect native decoder output to that interface. Prefer the same GPU device and native buffers; where an unavoidable GPU-only transfer/conversion is required, measure and report it instead of calling it zero-copy. No full-frame CPU readback or GUI-side color loop; no pixel serialization through widget state or language messages.
4. Bound buffer/resource pools and validate frame generation, target source, dimensions, per-plane geometry, color interpretation, synchronization and release order. Reject stale or unsupported handles before use. Completion callbacks must not unlock a different session or source.
5. Before default switch, verify actual video plus original overlays/PiP/input on Mac Studio, HO5 and cube; sequential and concurrent peer isolation; reconnect/context-loss/minimize/display changes; format/color/quality comparisons; matched-load CPU/GPU/copy counts and presentation latency; sustained/repeated lifecycle use. Retain every existing feature and signed rollback package.

## Exclusions

No Flutter migration, no mobile GUI rewrite, no online protocol change, no network identity reset, no signing-certificate change, no permission bypass, no Windows local capture/autostart change, and no mass replacement before acceptance. Any need to replace another graphics dependency or remove a supported capability is separately reported before adoption.

## Alternatives and rollback

An audited compatible upstream revision may replace some of this patch only after API/layout/performance and native-resource behavior are verified; apparent support on current main is insufficient. Flutter remains the user-proposed fallback if this route proves unacceptable, but changing to it requires a separate migration decision and cannot bypass native-texture engineering or the feature-preservation gates.

Work starts in an isolated source/build scope. A failed adapter is not deployed, and no partial product is described as a full release. Existing alpha.7 installations remain untouched until a complete candidate passes the agreed gates.

## Read-only hardware feasibility evidence (2026-10-02)

The same 1280×720 HEVC synthetic clip (60 frames, SHA-256 02c28ec5fcb611889b5d27471759cf8a486659d87ac6a79d313c1f88d26abcfa) was decoded locally on both target machines using their already-installed FFmpeg/runtime and existing device permissions. HO5 reported VAAPI frames through the radeonsi driver on /dev/dri/renderD128; cube reported D3D11 hardware frames. Both completed all 60 frames with exit code 0 and no reported decoding errors, retaining the hardware pixel format at the null output rather than producing CPU RGBA. No user screen was captured, no remote input/session started, and UU remained running with the same PID.

This proves usable local hardware-decoder capability for that synthetic HEVC format. It does not prove GPUI texture import, resource synchronization, on-screen presentation, all formats/HDR, 4K/60 performance, no copies, or an end-to-end product result. It supports testing a native adapter rather than assuming the machines require the current CPU pipe. Detailed logs remain on each host under .rp-router-unit-20261002/native-capability.
