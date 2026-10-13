# Desktop UI regression: cause and correction boundary

## Confirmed cause

The cross-platform work changed the default entry from `run_unified_gui(config)` to `desktop::run(config)` for macOS, Linux and Windows. The original Mac implementation can now only be reached through `REMOTE_PLAY_LEGACY_MAC_GUI=1`. This is an implementation replacement, not a missing skin, permissions issue, graphics-driver problem, or the user's configuration.

The original `app/src/ui.rs` uses GPUI/yororen_ui and imports `app/src/design_system.rs`; it contains product-specific capsule controls, the screen-first stage, device drawer, styled cards, pop-out surface and telemetry presentation. The current `app/src/desktop/mod.rs` uses eframe/egui, starts from `Visuals::dark()`, overrides a few colors and spacings, and uses a fixed non-fullscreen sidebar, separate session/status panels and standard wrapped controls. It does not consume the existing product design system. It retains some fullscreen auto-hide behavior but does not preserve the original overall layout and visual quality.

`docs/reviews/2026-09-29/FOUR-PLATFORM-GUI.md` documents the new default by alpha.4. Alpha.7 deployed that default to all three desktop OS families. Much of this sequence remains in the working tree rather than individual commits, so no invented commit attribution or exact initial change timestamp is used here.

The user requested complete cross-platform features and maximum implementation reuse. That is not approval to discard the established visual design. The substitution without equivalent visual migration and acceptance was a product regression introduced during the assistant-led cross-platform work, not a fault in the user's settings or a failure of the original designer.

## Evidence retained

The old product UI and design-system sources are still present. A read-only content snapshot of the original sources, current entry, current presentation and safety/model files is stored outside the repository at `/Users/jinliang/Workspace/.rp-ui-regression-20260930/baseline`, with SHA-256 values in `baseline-sha256.json`.

The external Antigravity walkthrough referenced by the design document currently contains Linux delivery notes rather than the original UI walkthrough. It was not treated as a valid visual screenshot baseline. The repository design specification and real UI source remain the available reference.

## Correction scope

Keep the current shared network/session/media/input and release-safety fixes. Restore the established visual and interaction design within a shared presentation implementation for Linux/macOS/Windows. A custom product appearance is the target, not a new design invented for this remediation. Do not reintroduce platform-specific divergent product UIs merely to make macOS look correct.

Required comparison states: idle and device discovery; network drawer; connecting/connected video; compact source/apps control; input lock/pause/error; transfers; windowed/fullscreen; small/high-DPI displays. Validate overlay bounds and pointer/keyboard event capture against the alpha.7 regression tests. Product screenshots must come from the actual new build; no static image or mock-up should be substituted for a working UI.

Do not flip the legacy GUI switch as an unverified fix. That path starts a separate older UI/runtime flow and has not been validated with all of the current shared session, instance and input safeguards. Do not reinstall alpha.2 merely to restore appearance and lose later functional repairs.

## This turn's actual changes

Read and compared both UI implementations, the entry-point diff, original design system and the migration report. Preserved the visual baseline and recorded migration invariants in AGENTS.md. No business/UI rendering code, installed app, launch agent, network membership, permissions or NAS package was changed in this audit. The visual restoration is not yet implemented or accepted; no claim of completion is made.
