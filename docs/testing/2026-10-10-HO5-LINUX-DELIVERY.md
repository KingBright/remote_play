# HO5 Linux fixed candidate — 2026-10-10

The shared input source was compiled and installed through a normal maintenance
switch of HO5's existing current user service. Same-user stop, atomic replacement,
startup and stable health checks passed with retained rollback. No live Portal
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
| Normal current maintenance receipt | `c6d5d8d6ed121b4d5e744acf5f8861ee06c1de60e3b57cb1ac6f178c1ab56c48` |

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
symlink at the guessed installation location. The previous binary reported
alpha.8/original GPUI/native video without embedded build identity. It is now
retained in rollback; the current binary matches the candidate and embedded
identity above. A disk query remains distinct from direct mapped-image proof.

The ordinary-user executor and current process are UID 1000. The independent
Remote Hosts Agent has a separate service scope and retained its original PID.
Preflight journal entries showed P2P candidate discovery without definite active
remote-control or transfer evidence. Live session counters were unavailable;
this was not treated as proof of inactivity. Continuing maintenance authorization
covers one rollback-protected same-identity switch and short connection rebuild.

The normal user-service stop succeeded with the existing signal/timeout policy;
old PID exit and profile-lock release were checked before atomic replacement.
Normal start succeeded. A distinct new PID retained the expected readable argv,
UID and service cgroup across six startup samples and the later read-only check.
The service is active/running, with zero restarts and exit status zero. Original
GPUI runtime renderer metadata and installed binary/compiled identity match.
The user unit hash is unchanged. Public profile bytes and private identity inode,
size, ownership, mode and mtimes are unchanged; private identity contents were
not read or copied. Protocol version remains 2 and persistent configuration
implementation is outside the source delta. Build-info now truthfully records
the installed verified candidate; its previous copy remains in rollback.

Hash-verified rollback copies retain the old executable, sibling build-info,
user unit and previous compiler output. A rollback transaction has not been
exercised. No published artifact, legacy system unit, OS permission, launch
configuration or persistent access was changed.

The receipt verifies controlled stop/placement/start and stable startup-path
binding. Direct mapped-executable access was previously denied by the OS and was
not retried or bypassed. It remains explicitly unverified; service launch binding
and installed metadata are not substituted for the native loaded-image gate.

## Native acceptance still required

Read-only Portal properties report RemoteDesktop version 2, available device
mask 7 and ScreenCast version 5. These are capabilities, not a session grant.
Startup logs show two WebSocket tunnel connected events and P2P candidate
observations for two peers, without connect-failure/disconnect events in the
checked interval. These establish transport events, not an accepted Session
handshake, authenticated GUI discovery, media to an identified receiver, actual
presentation or observable desktop input. Those layers remain unverified. The
prior native Host/App regressions passed (92/114 plus entry 1); they are not repeated here
and do not establish compositor input effects.

Using the upgraded current app, connect an authenticated viewer.
On HO5, use **Share this computer → Display + control**, choose the monitor in
the normal KDE selector and allow the requested keyboard/pointer access. On the
viewer, select that shared source in **Sources**, wait for an actual presented
frame, choose **Enable control**, then click the remote picture for keyboard
focus. Check target text, pointer, buttons and scrolling, then disconnect and
verify held inputs release. Record the current receiver and direction; ACKs,
uinput availability, decoded frames and UI submission do not prove effects.

The [four-platform matrix](2026-10-10-INPUT-STAGE-MATRIX.json) has twelve ordered
cross-platform directions (four platforms, host different from viewer), plus
one distinct-Mac observation record. Those thirteen records are physically
incomplete. The Mac record does not fix both device directions; each direction
needs its own controlled receipt when tested. The signed Mac input candidate
build 1082 was handed to its separate native upgrade task; the older 24/24 Mac
fixture result belongs to build 1081. Windows/Android registered-agent state
does not establish installed versions or physical acceptance. Audio, transfer,
direct/relay, source switching and sustained-session checks remain pending.

## Live product-entry investigation after maintenance

The current service remained healthy while its existing loopback `show` entry
received a four-byte activation request. No new profile or runtime was launched.
That protocol has no acknowledgement, so visible activation is not established.

Live D-Bus reads confirmed the ScreenCast and RemoteDesktop methods and their
previously reported capabilities. Corrected Portal tree introspection exposed
no session/request path under the current process's unique sender. This tree
is not an all-sessions API, so it does not prove every internal grant is absent.
The current profile has no Portal/restore-token filename. Compiled source uses
view-only ScreenCast persistence mode 0 and requests neither persistence nor
restoration for controlled RemoteDesktop. These are configuration observations,
not a successful or rejected restoration request; OS permission storage was
not read or changed.

This execution environment exposes no CUA/remote-desktop click tool. The existing
accessibility registry returned six applications, all successfully mapped to
PIDs, with neither the current product nor a KDE Portal dialog registered.
No product share button was invoked through that interface. The initial
introspection error was resolved by the successful direct registry read; it was
not reported as a Portal authorization denial.

The interaction stops at the GUI-entry capability boundary. No receiver was
occupied, no product sharing request was submitted, and no actual KDE selector,
choice list, input-permission prompt or response was observed. Session signaling,
source enumeration and first-frame/presentation acceptance therefore remain
unverified. A parent-side approved CUA session must operate HO5 and coordinate
an idle receiver before continuing the normal product flow. The user was asked
what actually appears after the `show` request; there is no evidenced KDE-confirm
step to ask for yet. Exact operation receipts remain in the private local handoff.
