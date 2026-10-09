# Bound source commands — 2026-10-09

Display/window selection and source discovery previously scheduled work against whatever connection was active when an async task ran. An old source button could therefore send a source ID to a newly selected device, including a different connection with the same source ID. Late error callbacks also lacked a connection/generation fence.

The product now carries a weak allocation binding and the device-network scope alongside the source list in the same locked Owner snapshot. Display buttons, application-window rows and the Apps control retain that binding from rendering. A stale button is ignored before releasing input or altering local menu state. Queued discovery and source mutation recheck it under the Owner’s existing session lock, covering the gap between a UI check and execution.

`remote_core::view_commands::ViewConnectionBinding` is a standard-library model with no renderer, network runtime or native handle. Its `apply` gate does not invoke a mutation for an expired/different allocation or scope, and retaining a binding does not retain the connection. The adapter still owns all actual sessions and resources.

Independent per-view command receipts fence queued source selection and discovery. Accepted select/close/disconnect/reconnect, device stream/files intents, and device-network actions invalidate both lanes. Selecting A, then B, then A again cannot revive an older queued selection even if A’s connection allocation was reused. The newest source selection wins before dispatch; source discovery does not cancel it. The source task reads the latest committed stream form values when it starts. Error callbacks require both their current receipt and original connection binding.

Existing current-session wrappers remain available for bounded acceptance fixtures. The production controls use the bound methods. Native video buffers, source revision/input readiness, capture-source validation, authentication, frame gates, files binding, audio, default GUI/backend, dependency pins, signing and profile formats are unchanged. This is an adapter/state fix; it changes no wire protocol or capture defaults.

## Verification

| Check | Result | Receipt |
| --- | --- | --- |
| Fixed model/backend regressions | 47 unique tests: 31 Rust + 16 Python, all passed, no ignored/skipped | `/var/folders/q_/0bykfl6x79zgp4fkvht8ddmc0000gn/T/remoteplay-gui-slices-wpbvmzzt/receipt.json` |
| New source binding/interruption coverage | Four production-model tests cover equal-label/equal-ID different allocations, scope/drop, newest source command versus discovery, selection round trips and cancellation | Same fixed receipt, `view_commands` slice |
| Actual GPUI adapter compilation and Owner checks | Exit 0; 4 Owner regressions passed | `/tmp/remoteplay-source-binding-owner-tests.{log,json}` |
| Recovery controls on final source | 2 production GPUI click/disable/back regressions passed | `/tmp/remoteplay-source-binding-component-tests.{log,json}` |
| Preservation | 105 prior dirty paths retain status/SHA-256; empty index before/after commit | `/tmp/remoteplay-ely-preservation.json` and guarded commit receipt |

**53 distinct automated tests** pass at this checkpoint; repeated runs are not added to this count. All Cargo work reused `/Users/jinliang/rust-target`, ran offline/locked with a bounded owned process, and retained over 4 GiB free. No cache cleanup, new target, private-key/config read, push, CI dispatch or deployment was performed.

The actual native recovery-component screenshots/interactions are recorded in `FIRST-CONNECTION-RETRY.md`. This internal command-binding slice changes no visual layout and does not add a separate native source-menu acceptance claim. The full product source chooser under authenticated remote sessions, cross-machine handshake, first displayed frame, reliable input/audio, full window/DPI states and sustained performance still need physical/native acceptance. No installed program or published artifact changed.
