# HO5 Linux fixed candidate — 2026-10-10

The shared input source has a real Linux release candidate. HO5's current user
service is still running the previous installation. No upgrade, live Portal
control request, physical presentation or target input acceptance occurred.

## Candidate and source binding

The existing HO5 target was reused with the locked, offline release build of
`remote_play_app`, binary `remote_play`, default features disabled and features
`gpui-restoration,native-linux-video`. Cargo finished in 3m 53s and recorded the
product artifact as `fresh=false`: the actual entry was rebuilt.

Seventeen input-related source files match primary checkpoint
`56a09bb8d62a2ca6fd0aa20be070e12935e70b43`. Other tracked files match the remote
base. The remote Git HEAD remains `0f858a5`; the two documentation files in the
primary checkpoint were not transferred. Compiled identity therefore records
that actual base plus the verified patch, rather than claiming remote HEAD is
the primary checkpoint.

| Evidence | SHA-256 |
| --- | --- |
| Source patch | `0a94f9f84a19b9cb5c00dc2e17eb67e5357eb43b2413237f2f4dd8dda7a14c31` |
| Source snapshot | `4ef98e373d933ea4e4a87f64a9bd230090b5c30c5452b8214c5fa22fd546799e` |
| Linux x86_64 binary, 47,573,520 bytes | `5dd7e1005395a30268cf99cfe347640f40aab65e6cf24aa32fb2505c55bed3f7` |
| Embedded build identity | `3ba025f11a0deca36cfcf786454c2ee2c6e136bd2061e7a7474ea099d5df6519` |
| Candidate verification manifest | `d03f676d86050f6c4b0cf77456ed6982caffee803d4c6386f21634157f9e6409` |
| Original remote candidate handoff | `3969d8c8a6c4b68b00d523293b9a6d92f881e6cc311a9b01de513cb29f56c0ec` |

`--product-info-json` reports `2.0.0-alpha.8`, Linux x86_64,
`restored-original-gpui`, original GUI compiled and native video compiled.
The actual binary matches the embedded source and build configuration.
Old-binary, wrong-source, wrong-configuration and runtime-sidecar relabel checks
all reject. Native dynamic dependencies resolve with no missing library.
Changed-source mtimes were refreshed only after complete hash verification and
before this build; source and identity inputs were checked again afterward.

This is a verified build candidate from a reused target. It has no fresh-target
observed-build release receipt and grants no publication authorization. Binary
and public build evidence were downloaded through Remote Hosts; the local
binary copy matches its remote size and SHA-256 and was not executed on macOS.
Private delivery paths, operation receipts and runtime details stay in the
local handoff, outside the repository.

## Existing installation and safe-switch boundary

The correct existing entry is the `remote-play-current.service` user unit at
`$HOME/.config/systemd/user/remote-play-current.service`, executing
`$HOME/.local/opt/remoteplay/2.0.0-alpha.8/remote_play`. There is no `current`
symlink at the guessed installation location. The installed binary reports
alpha.8/original GPUI/native video but has no embedded build identity. That disk
query does not prove the image mapped into the running process.

The latest read-only check found the user service active/running, enabled, with
zero restarts and exit status zero. Its executable and unit hashes remain
unchanged. Persistent profile inode, size, ownership, mode and mtimes remain
unchanged; private identity contents were not read or copied. Network protocol
version remains 2 and persistent configuration implementation is outside the
source delta. This is source/metadata compatibility evidence, not a completed
old/new installation transaction.

Hash-verified rollback copies retain the old executable, sibling build-info,
user unit and previous compiler output. A rollback transaction has not been
exercised. No published artifact, legacy system unit, OS permission, launch
configuration or persistent access was changed.

The existing process exposes no read-only active-session query. Current session
and transfer activity remains unknown; the user was asked to check the HO5
window. The normal upgrade is paused at that safety condition. OS access to the
mapped executable also returns permission denied; that limitation was not
bypassed. Service status and disk metadata are kept separate from actual
running-image proof.

## Native acceptance still required

Read-only Portal properties report RemoteDesktop version 2, available device
mask 7 and ScreenCast version 5. These are capabilities, not a session grant.
Discovery, signaling, media to an identified current receiver, actual
presentation and observable desktop input remain unverified. The prior native
Host/App regressions passed (92/114 plus entry 1); they are not repeated here
and do not establish compositor input effects.

After a safe identity-matched current upgrade, connect an authenticated viewer.
On HO5, use **Share this computer → Display + control**, choose the monitor in
the normal KDE selector and allow the requested keyboard/pointer access. On the
viewer, select that shared source in **Sources**, wait for an actual presented
frame, choose **Enable control**, then click the remote picture for keyboard
focus. Check target text, pointer, buttons and scrolling, then disconnect and
verify held inputs release. Record the current receiver and direction; ACKs,
uinput availability, decoded frames and UI submission do not prove effects.

The [four-platform matrix](2026-10-10-INPUT-STAGE-MATRIX.json) keeps all thirteen
directed physical acceptance rows incomplete. The signed Mac input candidate
build 1082 was handed to its separate native upgrade task; the older 24/24 Mac
fixture result belongs to build 1081. Windows/Android registered-agent state
does not establish installed versions or physical acceptance. Audio, transfer,
direct/relay, source switching and sustained-session checks remain pending.
