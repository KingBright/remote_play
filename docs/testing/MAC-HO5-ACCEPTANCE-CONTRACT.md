# Mac ↔ HO5 acceptance contract

This contract records pending physical checks; it is not an acceptance receipt.
The sole desktop baseline is original GPUI CE 0.3.3, Ely 0.1.1 and Blade 0.7.1.
No fallback toolkit, diagnostic owner or incomplete preview satisfies the gate.

## Prerequisites and ownership

Use the candidate branch `codex/gpui-mvvm-migration` from
`https://github.com/KingBright/remote_play.git`; record the full fetched commit.
HO5's existing repository is `/var/home/liang/workspace/remote_play` and its
existing build cache is `target`. Preserve configuration, device membership,
installed services and a verified per-device rollback binary.

MacBook's profile ownership repair belongs to its separate local task. Do not
duplicate it or read `mesh.secret`. MacBook was reported as alpha.7; Studio's
alpha.8 report is historical. Obtain fresh executable/signature/product-info,
PID and permission observations before testing. Cube was offline at the latest
inventory check. No Mac/Windows bidirectional physical acceptance has completed.

For Linux native video, verify FFmpeg 8.x headers and ABI 62/60 libraries before
setting `RP_VERIFIED_FFMPEG_INCLUDE`. Preserve platform video buffers and actual
capture color metadata; inspect encoded SPS where metadata changes. A successful
property setter, decode or UI submission does not establish presentation.

## Evidence per direction

Record device IDs, commit, SDK provenance, binary SHA-256, product-info, actual
running PID/executable/version, display resolution/DPI and test start/end UTC.
Separate source enumeration, capture, encoding, transport, decode, actual native
presentation, input, audio, file transfer, visual review and deployment results.
Missing physical checks remain unknown. Never substitute an installed old binary
or a test fixture for the candidate.

1. HO5 host → Mac viewer: enumerate screen and window sources, obtain approved
   portal capture, verify changing pixels in the actual GPUI window, and record
   screenshots of source choice, connected video and errors. Window capture's
   unsupported remote input must remain explicit; do not count it as a pass.
2. Mac host → HO5 viewer: verify native decoded buffers reach actual GPUI
   presentation with correct geometry, color, resize and fullscreen behavior.
   Confirm allowed keyboard, pointer and modifier input on the intended source.
3. Run two independent windows/sources or sessions where supported. Confirm
   device/source revision isolation; cancel one and verify the other continues.
   Reconnect and source switching must reject stale input and preserve identity.
4. Exercise audio, identity-bound file transfers, transfer cancellation, network
   errors and recovery. Preserve the full functionality catalog; describe each
   unsupported or untested capability explicitly.
5. Capture idle/network, connected, source-selection, transfer, error, windowed
   and fullscreen renders at representative DPI/resolutions. Review against the
   original design before broad deployment. Record a sustained run separately
   from first-frame success, including drops, latency, CPU/GPU and quality.

The report comparator checks supplied data consistency only. It does not prove
physical observations or authorize a signed release. Build, visual/performance
acceptance and installed deployment require separate receipts.
