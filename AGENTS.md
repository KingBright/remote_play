# RemotePlay release invariants

## macOS signing and deployment

- The authorized release identity is pinned in `deploy/macos/signing-policy.json`. A name such as `RemotePlay Local` alone is not sufficient: verify the exact certificate and designated requirement.
- Use `scripts/package_macos_unified.sh` for production packaging. A build machine without the existing signing private key may compile, but must return the binary to the authorized signing machine. Never add an ad-hoc fallback, create a replacement certificate automatically, or copy signing private keys through tools.
- Use `scripts/publish_desktop_nas.sh` to publish. Its macOS guard must pass before any upload. A checksum, successful codesign integrity check alone, or an agent's claim is not proof of authorization-compatible identity.
- Use `python3 scripts/install_macos_unified.py PACKAGE.zip` for a deployment plan and append `--apply` only for an authorized compatible update. Do not replace installed apps with a raw build or bypass the old/new identity comparison.
- Keep the canonical user app path and bundle ID stable. Preserve device membership and the existing launch agent. Retain a verified rollback app and record the actual running process/version separately from a NAS publication.
- Never reset or edit TCC, disable platform security, auto-approve user consent, or blame a missing permission checkbox based on a generic TCC error. Inspect the actual executable/signature first. A signature-compatible update does not grant permissions the user has not approved.
- A successful source list is not a captured/decoded/displayed frame. Keep signing, installation, source enumeration, first-frame, input, and long-duration acceptance separate.
- Do not mutate published version/build artifacts. Certificate rotation requires an explicit reviewed migration, not an environment override.

## Regression checks

Run `python3 -m unittest discover -s scripts/tests -v` for release tests. On the existing signing Mac, `RP_SIGNING_INTEGRATION=1` additionally runs native signed/ad-hoc/tamper tests using disposable non-running fixtures. No test resets system permissions or launches the fixture app.

## Desktop visual quality and migration invariants

- The desktop design baseline is the existing `app/src/ui.rs`, `app/src/design_system.rs` and `docs/UI_UX_SPECIFICATION.md`, not the default appearance of a replacement toolkit. Preserve the user's established product appearance when adding platforms.
- Sharing one implementation across Linux, macOS and Windows does not authorize a visual downgrade or make a minimally usable diagnostic UI a finished replacement. Changing the default GUI entry requires explicit disclosure and visual acceptance, not only compilation or connectivity tests.
- Restore the screen-first layout, compact capsule controls, device-management drawer, hierarchy, typography, spacing, semantic status colors, card treatment, overlays and appropriate transitions. Do not describe a palette-only change as full restoration.
- Preserve current routed-relay device isolation, source-revision/input safety, single-instance ownership, file-transfer identity binding, permission diagnostics and signed-release protections. Do not blindly roll back packages or switch on the legacy GUI to regain appearance while losing those fixes.
- Visual acceptance must use the actual executable rendering, with before/after evidence for idle/network, connected video, source selection, file transfers, errors, windowed/fullscreen and representative DPI/resolutions. Existing automated functional checks remain required. Generated mockups and hidden debug flags are not product acceptance.
- Do not mass-deploy a replacement visual implementation before its real screenshots and interactions have been reviewed. Report visual acceptance separately from build, stream, input and deployment status.

## Original GPUI restoration decision (2026-09-30)

The original GPUI 0.3.3 / yororen_ui 0.2.0 components have compiled and rendered in native windows on HO5, Mac Studio and cube. Restore that design on all desktop platforms; do not use target cfg as proof of framework incapability or choose a new default toolkit before this path is fairly evaluated. Follow docs/reviews/2026-09-30/ORIGINAL-GPUI-RESTORATION.md. Native video presentation must retain platform buffers; a CPU NV12-to-RGBA loop is not performance-equivalent. Probe success is not full product restoration. Never switch installed apps to the incomplete probe or revert secure Session/network fixes just to regain appearance.

## User approval and preservation contract (2026-09-30, latest instruction)

Simple/direct/useful interaction, platform-optimal measured performance, and no loss of existing functionality are simultaneous requirements. Use the union in docs/plans/GUI-PRESERVATION-CATALOG.json, not the current simplified UI as the entire product. It is an initial inventory, not proof of complete coverage.

Before substantial GUI/default-entry, native media, protocol/configuration, platform support, signing/identity or deployment changes, obtain explicit user confirmation of the concrete plan. docs/plans/GUI-RESTORATION-QUALITY-CONTRACT.md is a proposal, not approval. Prior direction to restore the original design is not blanket approval for any architecture change. Do not decompose a major rewrite into nominally minor steps to avoid this rule.

Do not equate decoding or UI submission with actual presentation; do not trade quality, reliable input, audio or existing features for a performance number. The report comparator is a consistency check on supplied data, not an external verifier or release authorization. Missing physical tests remain unknown. Preserve unrelated worktree changes and recoverable signed builds.

## Confirmed implementation approval (2026-10-01)

The user replied “好的请完成” to the concrete GUI-RESTORATION-QUALITY-CONTRACT.md plan. Scope A-D may now be implemented. This does not waive functional preservation, actual visual/performance acceptance, stable signing, or staged deployment. Changes outside that scope (Flutter, protocol/identity changes, full Android rewrite, system capture defaults) still require a new confirmation.

## Transferred source and native-video evidence (2026-10-03)

A source hash only proves source bytes, not that an incremental build consumed them. Transferred archives/Copy-Item can preserve mtimes older than an already-built target. After a completed, hash-verified delta and before any concurrent build, refresh only changed-file mtimes on the target (scripts/refresh_transferred_sources.py), then verify the intended new behavior or compiled provenance. Never claim cached build completion as evidence of unseen changes.

Keep actual capture color metadata through the encoder; never guess an unspecified YUV matrix merely to unblock a native viewer. Metadata changes require bitstream/SPS verification, not just a successful property setter. Source-color changes may require a new encoder generation. Native GPU backpressure can coalesce only already-decoded older outputs, never compressed reference frames. Retain at most one unsubmitted latest decoded frame rather than discarding the newest output whenever the bounded copy pool is briefly full.

## Approved Blade native interop (2026-10-04 continuation)

The user explicitly authorized the proposed minimal native-resource import and synchronization extension to the fixed Blade 0.7.1 dependency ("没问题，我授权，请继续"). This is approved together with the existing fixed GPUI restoration plan; do not ask again for the same scope. Keep versions pinned, preserve original UI and shared functionality, validate real presentation and input, and retain per-device rollback. The approval does not cover changing render engines, weakening authentication, dropping functionality or automatic changes to OS security.

## Actual GUI entry and release identity (2026-10-04)

The product default is the original GPUI, not desktop::run/egui. gpui-preview remains only a compatibility alias; no preview switch is needed for a correctly built product. Egui requires the explicit egui-diagnostic name and has a diagnostic window title. Missing original/native features must fail release checks rather than fall back silently.

The per-profile lock includes runtime renderer identity. A new original GUI must not report success by waking an alpha.7 unidentified/diagnostic owner; report the mismatch without spawning a second network runtime. This new metadata file is process state, not network identity; preserve all persistent configuration protections.

Use --product-info-json to inspect the actual compiled default without starting a window/profile. The release GUI gate verifies identity only; native screenshot and interaction acceptance are separate. Do not claim an installed program changed because a test/preview/staging main was compiled or launched. Versioned production packages and launchers still require explicit successful deployment receipts.
