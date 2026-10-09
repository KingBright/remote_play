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

## Follow-up: precise pre-window startup failure

The [bounded startup and metadata receipts](evidence/installed-startup.json) identify
a concrete application failure after the original access-card approval was reported:
the same canonical installed executable, PID 27007, exits **1 in 0.212 seconds**:

```text
Error: Io { path: "/Users/jinliang/Library/Application Support/RemotePlay/NativeMesh/mesh.conf",
  source: Os { code: 13, kind: PermissionDenied, message: "Permission denied" } }
```

Info.plist names the existing `remote_play` executable, mode 0755, arm64 on an arm64
host, with no background-only flag. Dependency inspection records system framework
references and `@rpath/libswift_Concurrency.dylib`. This launch reached the Rust
configuration read, with no reported dyld failure, before any GUI or capture. It
explains the missing-window observation; the earlier Computer Use invocation has
no app-exit receipt proving its own PID followed this exact path.

Caller UID/EUID is 501 (`jinliang`). The profile directory belongs to UID 501 with
mode 0700, but the existing 233-byte `mesh.conf` belongs to UID 0 (`root`), group
staff, mode 0600. Only metadata/ACL was inspected; no configuration content or key
was read, copied, recreated or changed. A metadata snapshot records the original
owner, mode, inode, size and mtime for a reviewable, reversible repair.

Exactly one noninteractive targeted repair was attempted. `sudo -n chown 501:20`
for this single file returned **1**, `sudo: a password is required`. Owner, mode,
inode, size and mtime remained unchanged. No password was collected or injected,
and no recursive chown, chmod, TCC edit or replacement profile was attempted.

The next minimal action is system administrator authentication for this exact
metadata-only repair on **MacBook**, retaining the current identity and 0600:

```bash
sudo chown 501:20 '/Users/jinliang/Library/Application Support/RemotePlay/NativeMesh/mesh.conf'
```

After confirmation, check owner/mode, then resume one bounded same-app start and
bind its actual visible window. Do not retry launches before this prerequisite
changes. This administrator boundary is separate from the already reported
Computer Use card approval. Replacing the app would not repair the unreadable
profile, so no package replacement or new signing was performed. The inspected
shared-target `debug/remote_play` path is absent; no full build was started.

Independent HO5 checks completed: caller and service PID use UID 1000; user service
is active/running; seat0 session is active local Wayland. Configured public artifact
SHA-256 is `4cd160cef5da7644486e7f46a695ad9753e8994820986c00723334a786adac2c`.
Its public `build-info.json` says alpha.8, `default_gui=restored-original-gpui`,
`native_video=true`, source ID
`67bf20c6295b898e95c803c7898ec7f5e547bb162bae6d3d7c90fe42dfd37c33`, and matches
that artifact hash. Operations `cd2982ef-16ba-4307-86a5-2dd49aac170d`,
`fe139efc-ebf1-4b03-b539-f396aa622d64`, `87f81bd1-5b21-476d-8f4d-9dc6062f903c`
and `b2b85e64-0f89-4652-b26e-cf474c26bd57` all exited 0 with sealed complete output.
These public artifact/manifest facts do not prove the protected running executable,
actual native presentation, source subscription, input or audio. No true direction
has started; all twelve cross-platform rows remain `not_tested`.
