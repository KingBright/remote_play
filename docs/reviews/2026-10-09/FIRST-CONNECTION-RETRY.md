# First connection failure recovery — 2026-10-09

The original GPUI/Ely device drawer now offers **Retry** and **Back to devices** after a first connection fails. A pending attempt offers **Cancel connection** and disables Retry. A failure no longer requires an already attached session to have a recovery target.

## Implementation

- `SessionTabsState` retains only the failed device/name/monotonic attempt, projects scalar failure/pending state, and distinguishes a foreground completion from a late background result using both the intent and the connecting attempt. Returning keeps the empty-selection marker so a background attachment cannot reopen an unrelated device.
- `OriginalOwner` retains the failed attempt’s media mode and current device-network scope. Retrying resolves current discovery/routes, uses the latest committed form values, and preserves files-only/silent mode. Scope changes expire retry targets. Two retry actions accepted before async scheduling reuse the pending generation, preserving one request and avoiding a spurious no-selected-device error. Offline/missing-route failures record a new intent without fabricating an endpoint or starting a connection.
- Typed connection failures and the existing per-view command receipts fence errors after a new selection, retry, cancellation or device-network action. No authentication, capture defaults, native frame/input gates, signing or persistent profile format was changed.
- `ConnectionRecoveryControls` is the production Ely component, inserted above the existing device list inside the management drawer. Stream and file failures return to that drawer; established-session Reconnect remains available.

## Verified results

| Check | Result | Evidence |
| --- | --- | --- |
| Fixed small regressions | 43 unique tests passed: 27 Rust + 16 Python; no skipped/ignored | `/var/folders/q_/0bykfl6x79zgp4fkvht8ddmc0000gn/T/remoteplay-gui-three-slice-j_6fhswg/receipt.json` |
| Production GPUI controls | 2 tests passed; actual component click dispatch, disabled repeat clicks, cancellation/back handler removal, old failure target | `/tmp/remoteplay-recovery-component-tests-final.{log,json}` |
| Owner adapter | 4 tests passed; pending settings, invalid settings, retry media/scope/generation, exact file-view identity | `/tmp/remoteplay-recovery-owner-tests-final.{log,json}` |
| Native fixture build | Exit 0; existing shared target; source hashes stable | `/tmp/remoteplay-recovery-native-build.{log,json}` |
| Native component rendering and interaction | Actual macOS GPUI/Ely window, PID 22613; Retry A creates attempt 2; two disabled clicks leave one Reconnect; choose B creates attempt 3; old A failure is Background; B failure offers recovery; Back clears target; new B attempt 4 is cancelled | CUA screenshot/action trace in this turn; `/tmp/remoteplay-session-recovery-native/01-retry-pending.json` through `05-cancel-new-attempt.json`, plus `provenance.json` |
| Native fixture shutdown | PID 22613 absent after Finish validation | Read-only targeted process check |
| Worktree preservation | All 105 prior dirty paths retain their status and SHA-256 | `/tmp/remoteplay-ely-preservation.json` |

Total: **49 distinct automated tests**, plus the native component interaction sequence. Initial test-only borrow errors were repaired. GPUI’s debug selector map retains removed selectors; the final test verifies the former hit area has no event handler instead of equating cached bounds with a visible button. Failed runs are retained separately under `/tmp/remoteplay-recovery-component-tests-first-failure.*` and `-selector-failure.*`.

The native window imports the production component directly and constructs only pure session state. It opens no profile, network runtime, remote connection, capture, or input injector. The temporary fixture bundle is separate from the installed product; it does not package, sign or deploy a release. The final small duplicate-scheduling adjustment changes only the Owner; the native fixture component and its compiled source bytes are unchanged. Native screenshots were observed through CUA and are in the tool trace; they are not exported screenshot files in this repository.

## Acceptance still required

This slice verifies local failure/retry controls, not the complete RestoredDashboard product appearance or real transport. Cross-machine handshake, captured/decoded/displayed frames, reliable input/audio, full drawer integration under real failures, windowed/fullscreen/DPI coverage and sustained performance remain unverified here. No installed RemotePlay version or NAS publication changed. The next independent main-chain gap is binding delayed source discovery/switch work and its errors to the exact selected connection.
