# Single-version authenticated sessions and measured clocks

## Scope and migration

Authentication/record version 3 and suite 1 are the only accepted paired session
protocol. The independent workspace command version remains 2. Retired enum tags
are retained only to decode a version-mismatch diagnostic; they cannot establish
keys or abort a version-3 connection. There is no automatic fallback.

Upgrade host, desktop clients, Android native bridge and test peers together.
Stop old processes before installing the coordinated release, preserving the
existing paired group, device identity, permissions and signing configuration.
Reconnect to obtain fresh ephemeral sessions. Backups/rollback must cover the
whole endpoint set; a mixed release cannot communicate. Protocol mismatch is a
signed error when the sender knows the group key; a nonresponsive older endpoint
produces a timeout that names the required version. Never trust unsigned errors
as permission to downgrade. Release/deployment is coordinated separately.

This change creates no persistent credentials. TLS certificate checks, rendezvous
and relay token validity remain their existing independent security policies.
An incorrect system clock can still prevent real TLS/token validation. Clock
measurement must not relax those checks.

## Threat model and establishment

The paired group PSK is the existing authority. A group member can impersonate
another member; this is not individual public-key device authentication and does
not provide forward secrecy. Local compromise or a malicious authorized host is
outside the clock-accuracy guarantee. Network attackers may record, reorder,
duplicate, delay, corrupt and inject datagrams.

1. Client generates a fresh 128-bit nonce and MACs version, suite and nonce.
2. Host verifies that MAC before allocating bounded state, then creates fresh
   128-bit server nonce and session ID and MACs the whole transcript.
3. Client verifies the challenge and sends a role-separated ClientFinish MAC.
4. Host verifies the pending transcript/address, local freshness and current
   native resource admission again, consumes the pending challenge once,
   installs the host-direction traffic state and returns ServerFinish.
   Client installs its traffic state only after that authenticated final flight.

All tags include a fixed protocol domain and distinct flight label. The full
transcript binds both nonces, version/suite and host-generated session ID.
The ephemeral session ID, not reusable client input, identifies resource owners.
Finish and resource commit have no intervening await. Existing live peers,
closing tasks and native-ownership receipts cannot be overwritten.

The local handshake lifetime is six seconds and never renewed by retransmission.
It uses monotonic elapsed time plus an independent local wall elapsed guard to
fail closed on observable suspend/backward/forward steps; no remote wall time
enters admission. `Instant` may exclude suspend on some platforms, which is why
local elapsed disagreement also invalidates an attempt. Stable cross-device
clock offsets of any size do not affect this check. This is not protection from
an adversary controlling both of the local clocks.

At most 32 address-bound challenges exist. A replayed client nonce cannot fill
multiple address slots. Host authenticated capacity remains 16; native connection
capacity remains its existing gate. Duplicate valid Finish returns a cached final
flight and never reinstalls traffic keys or resets counters/windows. Cache entries
expire, disappear on key rotation and are forgotten with revoked crypto.
Same-address rekey waits for the previous session/ownership expiry; normal new
connections bind a new UDP socket. Old Finish and old close IDs cannot replace or
clean a newer owner.

## Record replay protection

Each direction derives a separate ChaCha20-Poly1305 key from the group key and
complete transcript. Authenticated associated data carries version, session ID,
direction and a monotonically increasing sequence. The 96-bit AEAD nonce embeds
that sequence and cannot repeat under a traffic key. Sequence zero is invalid;
2^48-1 is the final permitted record. Reconnect rather than wrap.

The receiver uses a 4096-record sliding bitmap, allowing bounded UDP reorder.
Replay membership, tag verification and window commit are serialized. A bad tag
never advances the window. Lost/delayed control retries use a fresh record
sequence and existing business request IDs. Records beyond the reorder window
are intentionally discarded, even if they were legitimate.

A plaintext is limited to 8 MiB (the UDP reassembly limit is smaller). Each
receiving key permits at most 2^24 cumulative actual tag failures; successes do
not reset this count. Exhaustion permanently fails this crypto state closed for
send and receive, retaining the encrypted-peer requirement. It must never remove
a key and fall back to plaintext. These are conservative implementation limits
for large reassembled records, not a claim of TLS/QUIC protocol equivalence.

## Measurement and meaning

Authenticated probes bind the current session ID, strictly increasing probe ID
and the locally stored send time. The host records receive t2 at socket ingress
and send t3 immediately before reply construction. The client records t4 at
socket ingress. All frame/probe timestamps use one process-wide monotonic anchor;
wall time is sampled only to give that anchor an epoch-shaped numeric value.

For client send C1, host receive H2, host send H3, client receive C4:

- raw total RTT = C4 - C1
- host processing = H3 - H2
- estimated network RTT = (C4 - C1) - (H3 - H2)
- offset observation = ((H2 - C1) + (H3 - C4)) / 2

Raw RTT needs no synchronization. The host-processing subtraction assumes clock
rates are sufficiently close over that short exchange. One-way latency remains
an estimate: fixed path asymmetry and clock offset cannot be uniquely separated
from these four timestamps. For example a 4 ms forward / 12 ms return path looks
like 8 ms each under the symmetry estimate.

The bounded 64-sample / 60-second window selects low-RTT observations, then fits
centered offset plus drift. At least four selected observations spanning three
seconds are required before Synchronized. Fit residual CI, an independently
propagated RTT/2 asymmetry allowance, assumed holdover wander and quantization
are separate diagnostics. Their sum is a model allowance, not a certified
accuracy bound; a zero residual CI does not prove absence of asymmetric bias.

Unknown, acquiring, stale, invalid and impossible negative frame mappings are
unavailable. UI labels describe capture-to-client-receive estimates, excluding
client decode, rendering and display scanout. Invalid measurements are not
clamped into a plausible zero. Raw host checkpoints remain diagnostic data.

The bridge's legacy `latency_ms` now means local decoder cost only. Its new
`capture_receive_estimate_ms` and clock diagnostics are nullable, and Android
preserves null rather than treating missing measurements as zero. The desktop
capture-to-receive display uses that explicit availability state.

The older Mac renderer also no longer subtracts wall-clock/RTP timestamps.
Its auxiliary cumulative-stage estimate excludes decoder-queue wait (not yet
instrumented in that pipeline) and actual display, and is labelled accordingly.
Taking a decoded frame into a UI slot cannot fabricate GPU/VSync completion:
`render_done_ts_us` stays unavailable, including in the older desktop UI.

Pending probes are bounded to 16 and expire after five seconds. Replies must
match a pending ID and exact echoed t1 and are consumed only once. Estimates
expire after ten seconds. Active views sample each second; paused/idle views
retain sparse five-second traffic. Reconnect/session changes reset the model;
local continuity loss or a major observed clock/path step invalidates the old
fit and reacquires without reusing probe IDs. Clock failures never decide
permissions, input leases or whether authenticated traffic proves liveness.

## Verification and deployment matrix

Cloud fixed guarded modes cover protocol serialization, handshake/record
adversarial cases, four-timestamp mathematics, affine drift, asymmetry, stale
samples, ingress timestamps, encrypted replay, native owner admission/teardown,
initial Open injection, lossy final-flight retry, P2P routed isolation, publisher
permission behavior and unavailable UI/JSON state. Test execution evidence is
reported separately; source tests alone are not a pass.

Before release, exercise matched host/client builds on Windows/macOS/Linux/
Android: clocks offset by minutes/hours/day, ±100 ppm drift simulation, 4/12 ms
asymmetry, queue delay/loss/reorder, duplicate flights/records, reconnect at same
and new address, old Finish/close arriving late, sleep/resume, local wall steps,
PSK rotation, native stop pending/failure, paused keepalive and idle/recovery.
Check certificate/token failures still fail normally. Confirm capture-to-receive
is unavailable until fitted and never advertised as actual display latency.

## Primary references

- [RFC 5905 §8 and §10](https://www.rfc-editor.org/rfc/rfc5905.html#section-8):
  four timestamps and low-delay clock filtering; this implementation is not NTP.
- [RFC 7679 §3.7](https://www.rfc-editor.org/rfc/rfc7679.html#section-3.7):
  one-way delay uncertainty from clock offset and synchronization errors.
- [RFC 8446 §4.4.4](https://www.rfc-editor.org/rfc/rfc8446.html#section-4.4.4):
  transcript-bound Finished authentication; used as a design principle only.
- [RFC 9147 §4.5.1](https://www.rfc-editor.org/rfc/rfc9147.html#section-4.5.1):
  record sequence replay detection and authenticating before window updates.
- [RFC 9001 §6.6](https://www.rfc-editor.org/rfc/rfc9001.html#section-6.6):
  distinct confidentiality/integrity AEAD usage budgets.
- [Rust Instant](https://doc.rust-lang.org/std/time/struct.Instant.html):
  suspend behavior is platform-dependent.
