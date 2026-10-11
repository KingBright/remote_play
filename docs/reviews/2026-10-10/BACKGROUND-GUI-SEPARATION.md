# Background service and independent GUI lifecycle

This isolated implementation is based on
`c4d7031e29e5aa11d18c92e8954603debd394194`. It preserves the shared
product checkout's 88 pre-existing changes. It does not modify the installed
MacBook app, daily launch agent, Remote Hosts, other devices, TCC or persistent user keys.
The RP source task handed ownership of this independent implementation to this
task and explicitly said it would not start/replace a Mac app.

## Finding and implementation

The installed alpha.7 and signed alpha.8 candidate have headless execution but
no service attachment protocol. The current candidate's GUI and headless mode
claim the same `desktop-instance.lock`; its explicit test rejects a GUI beside
the headless owner. Setting `REMOTE_PLAY_HEADLESS=1` alone is therefore not an
independent GUI/background solution.

The new macOS entrypoints are `--background-service` and `--gui`. The former
claims the canonical profile lock, owns passive hosting, discovery/P2P/relay and
profile changes, and never initializes GPUI. The latter authenticates a private
same-user Unix socket, compares protocol/version/profile and executable SHA-256,
uses a separate private window lock, and does not register or bind a second host,
discovery, relay or P2P runtime. Its existing outgoing authenticated sessions and
native video/audio/input implementation stay in the GUI process. Pairing changes
delegate to the background owner. GUI attachment only reads existing membership.

A primary attached window's close callback releases input/sessions and exits
GPUI; a standalone legacy window retains its previous behavior. A service outage
or generation change invalidates frontend sessions. The private empty
`background-service.enabled` role marker prevents a default GUI launch from
falling back to standalone hosting during service restart. No frontend launches
the service or another GUI; KeepAlive belongs to the external service manager.
Windows/Linux deployment of these new roles is explicitly unavailable pending
their own transport/portal and platform acceptance; existing entrypoints remain.

The product binary and review executable share `entrypoint::run`. The review
example adds only a loopback authenticated connection probe and uses the actual
production GPUI. `--background-status-json` reports safe process/build identity
without invitation codes. IPC frames are bounded; clients and endpoints are
restricted to the profile owner and private directory/socket permissions.

## Evidence

The final review executable SHA-256 is
`0fb003334f063ba59a8c4f8cc3d2a199136bfde90d6ca9f6bd2663b6a134d7f1`.
It is an unsigned development fixture, not an installed or signed release.

Application library tests: 115 passed, 2 existing ignored cases. Release Python
checks after build-identity dependency and installer integration: 263 total,
256 passed and 7 existing signing/platform skips. Four launch-policy migration/rollback tests reject env-only separation, preserve the
existing profile/display identity, and refuse concurrent rollback edits. Private
socket/frame/mismatched-executable tests and actual frontend ownership tests pass;
frontend attach/drop leaves identity file inode, size and mtime unchanged.

In the final process fixture, the native GPUI rendered 34 times and exited with status 0 through the
existing five-second product acceptance hook. Authentication to the loopback
host succeeded before GUI startup, after GUI exit, and after deliberately killing
and explicitly restarting only the disposable background process. A duplicate
background was refused. Default App launch during the restart gap failed without
creating a second host or GUI. The recovered service exited cleanly; no fixture
process remains. Receipt and render evidence are retained in the task workspace:
`lifecycle-final-process-evidence/process-receipt.json` and
`gui-render-receipt.json`. Run the portable helper with a fresh evidence directory:

```sh
cargo build --offline --locked -p remote_play_app --example background_lifecycle_review
python3 scripts/acceptance/separated_lifecycle_review.py \
  --binary EXISTING_TARGET/debug/examples/background_lifecycle_review \
  --evidence-dir /absolute/new/private/evidence --timed-gui-exit
```

The first manual-close fixture did not pass. The native UI tool timed out, the
bounded fixture stopped its own processes, and its failure receipt was retained.
No manual close-button screenshot/interaction acceptance is claimed. The normal
exit integration hook proves actual process separation, not human-driven close
or the installed launchd policy.

## Deployment and remaining acceptance

`scripts/background_lifecycle_plan.py` produces a pure reversible launch-agent
plan. It preserves label `com.remoteplay.host`, canonical executable/profile and
existing environment, changes ProgramArguments explicitly to
`[canonical-executable, --background-service]`, and keeps KeepAlive/RunAtLoad true.
The headless environment is supplementary; compiled explicit role capabilities
are required. Rollback requires both the previous signed app and original plist,
not only one component. The planner executes no system operation.

The guarded installer now offers an explicit `--separate-background` plan/apply
option. It verifies the signed candidate's real role capabilities before stopping
anything, backs up the original raw plist privately, and holds the profile lock
while moving the signed App and atomically changing the launch role and marker.
The journal binds app identity, service label, plist hashes and original mode/group.
Background health checks bind the actual service PID, executable hash and profile.
Rollback restores the prior signed App and exact original plist, preserves the
prior marker state, holds the profile lock across the App/plist restoration,
and restarts only that service. Unknown command outcomes,
concurrent edits and tampered backups refuse automatic replay.

The opt-in fixture mode is confined to a fresh private temporary root and an
independent `com.remoteplay.lifecycle-test.<uuid>` label. It uses the unchanged
pinned signing policy. Three disposable signed fixture archives exercised actual
launchd migration and known-failure rollback. No daily installation changed.
Old signed candidates lacking the new roles remain ineligible for the new plist.

The complete real launchd fixture passed: GUI normal exit through the existing
integration hook without respawn; background authentication before/after exit;
SIGKILL of the verified owned service followed by launchd recovery of only the
background; duplicate host refusal; default GUI refusal during a controlled
unloaded gap; reattachment after service recovery; and signed upgrade failure
restoring the exact previous App/plist tuple and working connectivity. Identity
bytes and launch-agent owner/group/mode were preserved. All test labels were
removed. Raw roots, labels, PIDs, ports, logs and receipts remain local.
The final revalidation used a mode-0644 launch agent beside mode-0600 role
markers and preserved those distinct modes through migration and rollback.

Initial retained failures identified macOS directory-group inheritance and an
acceptance-helper environment mistake. Neither was a permissions failure or a
reason to alter file groups. Atomic replacement now checks the original group;
the manual GUI gap probe does not inherit the service's headless environment.
The first manual-close UI approval remains pending; no alternate UI tool was used.

The combined observed-build candidate from
`f6cbe7b627663185e0a0064901c8d2d1bd7b03cf`, signed build `20261010.1081`,
subsequently passed all 24 real identity-bound installation checks. Independent
compiler/receipt/source/manifest anchors, native mapped backend, exact release
state persistence, older-build and wrong-manifest rejection, and a real SIGSTOP
health failure followed by verified complete rollback were exercised without
replacing any validation hook. This establishes the joint candidate's private
installation transaction, not production installation or publication.

Still required: actual native close-button/quit interaction and login/reboot without opening GUI,
two-Mac connection/video/input/file/pairing/relay acceptance, and Windows/Linux
role support. No OS permission or credential changes are needed or authorized by
the fixture evidence. The installed alpha.7 service still has KeepAlive=true and
HEADLESS=0; its existing restart behavior has not been silently disabled.

## Build-identity integration boundary

The independent source task published `bd242a4381abe63ac51589c27aab7c17e18b30d3`
with `scripts/build_identity_gate.py` and `docs/testing/BUILD-IDENTITY-GATE.md`.
This task inspected that exact committed tooling without modifying its five
owned files or the shared worktree. The lifecycle branch's local original
`69b7a5f26b567af27cd52f2625f989ae8e2c9151` maps to the sanitized publication
commit `ffe96bcd5f6ebe47b6a55cb5a3329f726cc42348`; the original reference remains
local and is not published through this branch.

The producer subsequently implemented immutable embedding in
`0f858a51773a5b5309291a78f6ef48f683255682`. This task read that interface without
editing its producer files. The retained lifecycle binaries remain unstamped;
their acceptance does not establish a combined observed-build release chain.

The coordinated release integration must combine the generated module in
ProductInfo with these lifecycle fields and have the authoritative packager preserve the observed build
receipt and seal an external `remoteplay-release-manifest.json`. The existing
publication signing guard still rejects that archive-root file and needs coordinated
manifest handling. The installer first verifies the complete independently pinned
archive, then uses a bounded App-only view with the unchanged signing guard.
The installer now verifies the archive against the independently supplied
commit, build-identity digest and manifest digest before any stop. It journals and
backs up the canonical manifest/expectations with the App/plist, installs them
under the same profile lock, and restores all components on a known failure.

The core `verify_install` API deliberately requires the configured frontend
arguments `--gui`. The launch agent here correctly runs `--background-service`.
Calling that API with invented frontend arguments would give false launcher
evidence. The installer-owned background adapter now uses the actual observed
launch-agent arguments and authenticated service PID, then the independently
hashed native observer verifies the loaded background image. Frontend launch
and running-image acceptance remain a separate observation. See
[INSTALLER-BUILD-IDENTITY.md](../../testing/INSTALLER-BUILD-IDENTITY.md) for the
five input fields, exact release-state rollback contract, older native fixture
checks and the completed 24-check joint signed-product release-state transaction.
No missing source/compiled evidence is defaulted to passed.
