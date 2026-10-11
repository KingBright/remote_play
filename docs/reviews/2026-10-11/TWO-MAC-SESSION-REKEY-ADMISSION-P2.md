# Retain native teardown admission across key rotation

Review input: `independent-remoteplay-p2-045223Z`, base `e8a7c37e8bdff3cf9b751da79c5aa7aa5a85be96`.

A key rotation clears authentication while live failed/unknown close receipts remain. Previously, Hello checked those resources only inside an existing authentication entry. A valid new-key Hello from the same address could therefore delete the live receipt and reset the eight-connection budget.

Hello now checks active peers, closing tasks and live closed resources independently of the authentication table, before installing a replacement identity or deleting a receipt. MAC/timestamp verification remains first; valid same-nonce retransmissions still replay the existing salt/time without deleting any receipt. The closed resource slot survives cleared authentication and repeated attempts. Only actual worker/native release makes it eligible for removal. Open and legacy StartStream share their existing native-ownership and total-capacity gate.

## Paired behavioural regression

`key_rotation_same_address_hello_keeps_pending_native_budget_until_release` uses the production Hello/admission functions, synthetic keys and a withheld stop fixture. The baseline run preserves the old conditional guard; helper extraction only makes that actual path testable. It fails after a valid same-address new-key Hello returns success while native cleanup remains pending (exit 101). The identical test body passes after moving the guard outside the authentication lookup (exit 0).

It checks nonce replay, changed nonce, revoked-key MAC and stale timestamp rejection, clearing authentication for rotation, a retained owner plus seven active peers reaching capacity, continued rejection by both Open/StartStream's shared gate, and delayed successful owner release permitting a new eighth peer. Additional regressions cover a closing task with cleared authentication, failed stop with pending=0 but native_streams=1 across repeated attempts, and worker-only occupancy until the worker guard drops. No real device key, capture stream or profile is modified.

## Validation and scope

- Exact paired test: red 0 passed/1 failed; green 1 passed, both compiled successfully.
- Host: 156 passed, 1 ignored, exit 0.
- Background IPC: 3 passed, 117 filtered, exit 0.
- Release Python: 271 tests, 7 skipped, exit 0.
- Targeted rustfmt and git diff whitespace checks pass.

Task-local raw and compact logs, paired-test hash, final source inventory and commit are preserved in the sibling `two-mac-session-recovery-rekey-p2-20261011` evidence directory. The red instrumentation patch/source are retained so the baseline is reproducible. These are local fixture regressions, not physical ScreenCaptureKit or installed two-Mac acceptance. No upload, formal candidate build, signing, installation, permission change, remote command or deletion was performed in this round. Earlier candidate/archive/handoff and real-stream exit-1 evidence remain separate and unchanged.
