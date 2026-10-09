# Real-device checkpoint — 2026-10-09

Physical directional tests have **not started**. MacBook↔Mac Studio, Mac→HO5 and
HO5→Mac remain `not_tested`: there is no authenticated source subscription,
decoded/visible frame, screenshot, audio, input, reconnect or concurrent-window
receipt from this phase. The earlier synthetic loopback remains a separate record.

The [small raw checkpoint](evidence/real-device-checkpoint.json) records exact
installed binary hashes, signature checks, operations, exits and the access blocker.
The [directed matrix](../../testing/FOUR-PLATFORM-MATRIX.json) remains the unified
coverage record. Current source checkpoint: `2d3e854`.

| Device | Confirmed instance | Remaining boundary |
| --- | --- | --- |
| MacBook-M2-Max | Canonical signed app, metadata 2.0.0/build 20260930.7; no `remote_play` process before selection or at the checkpoint | Older installed artifact; compiled renderer cannot be inspected with `--product-info-json`, whose flag is absent. |
| Mac Studio | Canonical signed app, metadata 2.0.0/build 20261004.80; PID 23896; public checks exit 0; operation `b6b3ef10-d9e1-4515-989f-9af3137ac718` | Current candidate is not deployed; exact renderer, existing capture/input consent and live source are unconfirmed. |
| HO5 | User service `remote-play-current.service` active/running, PID 1520582; configured executable path names alpha.8; operation `7edce8e9-dfcb-4419-938b-d87e05f7d512`, exit 0 | Configured path does not prove running bytes. Earlier `/proc/.../exe` read was denied; runtime hash/version and active display details remain unknown. |
| cube / Android | Previous offline/USB-unauthorized blockers retained | No repeated wake, USB approval, pairing or installation request. |

Both Mac apps passed integrity **and** the pinned exact leaf-certificate requirement.
This proves the observed installed signing identities only. It does not prove current
GUI identity, permission grants, compatible deployment, capture or presentation.
The initial inline `codesign -R` invocation omitted `=` and was invalid; its exit 1
is retained as a method error and corrected by the public signature read above.
The system-scope HO5 unit query likewise does not describe the running user service.

## Existing application-access card

Computer Use selection of `/Users/jinliang/Applications/RemotePlay.app` stopped at
**`Allow Computer Use to use "RemotePlay"?`**. The parent verified the original card
`manager:1dd13b5ff994418c871a3db1bd305548:2`, message
`Sentinel_00c2ed3431288191adad4afa6cee8b77`; the user subsequently reported approving this original card.
The tool returned `bounded installed-app selection timeout` after 2715.6563 seconds,
despite a 10-second tool limit and 5-second Promise limit. These limits did not bound
the platform approval wait. The original card and CUA session were preserved through that wait.
After the reported approval, inventory showed the original binding absent and app
not running. One binding recovery for the same canonical app returned Computer Use
server error `-10005: timeoutReached` in 5.962 seconds. Subsequent `pgrep` returned
exit 1 with no process. No new access card or replacement UI path was requested.
The chat approval does not prove successful tool access; no window was obtained.

This access card is separate from OS screen-capture and accessibility permissions.
Neither OS permission has been verified or changed. No permission dialog was
accepted, no private configuration/key was read, no identity was created, and no
installed app, launch agent or service was replaced/restarted. No new build or
synthetic loopback was run. There is no durable real-image artifact to claim.

The current blocker is **same-target native application binding timeout after the
reported approval**, rather than a fresh permission request. Preserve the recovery
Promise/session and inspect its final outcome before any additional launch. Obtain
the actual installed window first. Continue the explicitly selected real-window baseline using existing
consent and normal product controls; classify missing consent/source/session as a
blocker. Do not count old installed artifacts as current GPUI/MVVM acceptance.

The matrix consistency check exits 0. The complete cross-platform gate remains
exit 1 because all twelve physical cross-platform directions are unaccepted.
These checks validate supplied records, not an external display or release.
