# Two-Mac session recovery

## Evidence and attribution

Installed build 20261011.1085 remains untouched. Studio media to MacBook over the existing Relay route decoded 53 native frames, with source revision 1 and no input sent. Unsubscribe was acknowledged, Close was not; exit 1 is retained. The reverse MacBook-media-to-Studio attempt selected the existing P2p route and timed out in authentication, before Open or Subscribe. Its exit 1 is retained.

Both installed binaries and actual process images matched SHA-256 `16204e3821ca0d7e45c094d9e1fb5214b95585212e0ce87574429313bfef7f78`. Later read-only audits found both advertised loopback route sockets owned by the expected backends. Existing P2P log lines had no timestamps and successful authentication had no stage logs. Those audits cannot assign a cause to the earlier failure.

A synthetic loopback rendezvous reproduction proved a separate P2P defect: each peer route stored one last local UDP sender as its return target. A reverse Hello reached the prior viewer instead of the host. This is a confirmed implementation defect; its causal role in the real reverse timeout remains unconfirmed until a bounded same-route candidate test. Do not blame credentials, TCC, identity or NAT from this evidence.

## Implementation

Production P2P now uses the existing authenticated, device/connection-addressed routed envelope over the direct P2P transport. Each viewer token has its own host-facing UDP endpoint and crypto owner. It derives the routing key from existing membership in memory, retains SessionHello/Accept and AEAD verification, and does not select a different transport or fall back to unaddressed packets. Two clients and a reverse connection authenticate independently in the isolated test. Router cancellation owns and joins its child tasks.

Both ends must use this P2P change. A mixed 1085/new pair fails closed on the addressed P2P path; no backward-compatible unauthenticated fallback was added. LAN/Relay transport selection and credentials are unchanged. This candidate has not been deployed to any device.

Host Close previously removed a Connection without sending the already-defined Closed reply. The existing client example waits for Closed, so no protocol evidence supports treating the observed missing reply as success. Subscription Drop only requested cancellation/abort, which did not establish completion of native teardown.

Close now removes the connection from ingress, cancels and joins capture/audio/clipboard/file/talkback/reporter/sender workers, and waits for synchronous ScreenCaptureKit stop completion and object Drop before success. Teardown runs separately from the host control loop. A stop failure or worker panic yields Error, never Closed. Duplicate Close receives a retained terminal receipt for that address and ID; mismatched IDs cannot close another session. Close/Closed are included in authenticated client retry tracking. Unsubscribe waits for its capture and retiring audio tasks before acknowledgment; its wait is bounded. Disconnect/key rotation use the same teardown path. Cancellation does not claim a successful terminal receipt.

Read-only same-user IPC accepts `{"InspectConnection":{"connection_id":N}}`. It exposes only the specified ID's scalar lifecycle records: state, capture/audio workers, ScreenCaptureKit stream count, successful stop completions/errors and cumulative capture/encode frames. Closed records expire after 60 seconds; colliding IDs remain separate and cannot be inferred as one session. No frame bytes, titles, pairing material or input are exported. Handshake logs add UTC-compatible epoch timestamps, fixed stage labels, peer ID and local port only; never secrets, nonces, salts or MACs.

## Verification and rollout gates

The known-ID inspection script requires a pinned backend SHA, a successful native video start/stop, zero active capture workers/streams/errors, a Closed state and unchanged counters across two samples. Unknown IDs, collisions, missing counters, wrong backend or continued capture fail closed. A Closed control reply alone is not sufficient for physical resource acceptance.

Use the existing packaging/signing/identity/installer scripts and canonical app paths for any subsequent approved candidate rollout. Do not replace an installed app with an unsigned compiler output. Preserve the 1085 rollbacks and all raw failed probe evidence. Upgrade and verify both Macs before a new addressed-P2P test, then reuse the bounded headless consumer (video only, no GUI/audio/input, exact selected display, strict existing-profile load). Pin the observed candidate hashes in the wrapper, keep the reverse test on P2p and stop at the first terminal failure; do not force Relay to hide the issue. Record both ends' timestamped route/auth stages and query the media host's known connection ID immediately after Close. Require native stop completion, zero counters and stable frame totals as well as decoded frames. Any remaining authentication failure requires diagnosis from those new stages before another retry.

Windows cube, HO5 and Pixel are outside this candidate/test scope. No device authorization, membership, TCC or production security state is changed by these source fixes or fixture tests.

Fixture verification: host 145 passed / 1 ignored; remote_core 292 passed; background-service IPC 3 passed; release Python suite 271 tests / 7 skipped. These are fixture/compile checks, not evidence that installed 1085 released native resources or that the real P2P timeout is fixed. No GUI presentation or input acceptance was performed. Native Linux portal RPC completion and Windows physical device behavior were not exercised by this Mac-only candidate.

The unknown-source regression injects an empty catalog only under cfg(test), requests a nonexistent window while a capture fixture is active, and verifies the old source/subscription and worker/stream counts remain intact, with no new audio group or retirement. It exercises subscription admission without querying the OS catalog or requesting capture permissions.
