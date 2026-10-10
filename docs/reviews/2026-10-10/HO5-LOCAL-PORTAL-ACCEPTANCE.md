# HO5 linked candidate and portal acceptance attempt

This redacted summary records local results through 2026-10-10 09:16 UTC.
Raw paths, process/operation IDs, environment values and logs remain local.
The [machine-readable summary](evidence/ho5-local-portal-acceptance.json)
contains reproducibility conditions without those private details.

## Candidate and rollback

The native product and portal control example linked successfully at 08:55 UTC,
using locked/offline builds, the existing target and verified FFmpeg headers.
The candidate was built from
`c4d7031e29e5aa11d18c92e8954603debd394194`, SHA-256
`6d742360ed795a7f09b115bbb1f3d6ce8994ae6dc1139c153d9583761b8b4658`.
It reports alpha.8, restored original GPUI and compiled native video; it was not
installed or opened as a GUI. A later documentation commit does not relabel it.

The previous disk artifact has SHA-256
`4cd160cef5da7644486e7f46a695ad9753e8994820986c00723334a786adac2c`,
matching the retained Linux package. A rollback copy was hash-verified in
separate local staging. Service/launcher/configuration remained unchanged;
profiles and secrets were not read or copied. Actual process executable access
was denied, so the loaded process version/commit remains unknown.

## Normal portal requests

The compiled `host/examples/linux_portal_control.rs` made normal sequential
selection requests:

| Request | Start UTC | Bounded wait | Result |
| --- | --- | --- | --- |
| Window | 08:57:16 | 25.004 s | Test-owned SIGINT; child exit 1; cancelled |
| Monitor | 08:57:41 | 25.006 s | Test-owned SIGINT; child exit 1; cancelled |

Neither obtained a successful desktop-user choice. No forced kill, fabricated
portal response or automated consent occurred. Harness exit 0 means the attempt
completed; it is not capture acceptance. Captured frames: **0**. The source-control
example itself does not prove capture, decode or presentation. Its cancelled
output does not expose the exact portal phase or confirm external dialog/session/
FD cleanup.

## Read-only desktop and identity follow-up

At 09:14 UTC, logind reported an active local KDE Wayland session. The user
service manager had Wayland/display variables; the agent daemon lacked its
display variables. No environment was changed, and that difference alone does
not prove why a portal request did not finish. ScreenCast version 5 was available.
At 09:16 UTC, the actual KDE portal unit, portal frontend, PipeWire and WirePlumber
were active/running. No portal control processes remained.

The tool session exposes terminal access, but no HO5 desktop screenshot/control
interface. Earlier selector visibility therefore remains unknown; later service
metadata cannot establish it. No further blind request was sent. Successful
capture now requires a local desktop user to choose through the actual selector.
Revocation after capture, stop/restart, changing frames, multiwindow isolation,
native GPUI presentation, input/audio/files and sustained performance are untested.

The existing `scripts/verify_desktop_gui.py` gate was run on hash-verified artifacts:
the candidate passed with no network/window startup; the old disk artifact was
rejected because its metadata still advertises the legacy diagnostic GUI.
Both report alpha.8, so version text alone does not identify the candidate.
ProductInfo and profile-owner metadata omit compiled commit/build; a complete
startup provenance gate remains unfinished. These checks do not verify the
loaded process or provide visual acceptance.

The UI deployment hold remains. No installed app was replaced or service
restarted, and no Mac App was launched/maintained. The earlier Python result is
**197 total, 191 passed, 6 skipped, 0 failed**; tests were not repeated.

[Git access and guarded fetch](../../testing/HO5-GIT-CANDIDATE-ACCESS.md).
