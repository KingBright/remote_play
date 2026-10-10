# Background service and independent GUI lifecycle

This isolated implementation is based on
`c4d7031e29e5aa11d18c92e8954603debd394194`. It does not modify the shared
shared product checkout, its 88 remaining changes, the installed
MacBook app, launchd, Remote Hosts, other devices, TCC or persistent user keys.
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
checks: 193 total, successful with 6 existing signing/platform skips. Four new
launch-policy migration/rollback tests reject env-only separation, preserve the
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

No production installer changes or launch-agent migration have been applied.
The existing installer currently requires ProgramArguments `[executable]` and
preserves the plist hash; it needs a reviewed journaled role/plist migration and
compatible rollback integration before deployment. Old signed candidates lack
the new roles and cannot safely be used with the proposed new plist.

Still required: signed packaging under the pinned existing identity, actual
native close-button/quit interaction, real launchd crash recovery, login/reboot
without opening GUI, guarded single-instance upgrade and app+plist rollback,
two-Mac connection/video/input/file/pairing/relay acceptance, and Windows/Linux
role support. No OS permission or credential changes are needed or authorized by
the fixture evidence. The installed alpha.7 service still has KeepAlive=true and
HEADLESS=0; its existing restart behavior has not been silently disabled.
