# Local service contract v1: first integration slice

## Implemented in this slice

`app/src/background_service.rs` now dispatches a real `Negotiate` request on the
existing private framed JSON endpoint. `Client::connect` first reads and validates
its existing Snapshot, then negotiates before constructing a frontend. Polling
and pairing changes use serialized cache transitions. A new service generation
requires new negotiation; errors invalidate the capability cache. Snapshot and
capability state commit together after membership refresh succeeds. Status reads
remain available while IPC is in flight.

The offer carries `version: {major, minor}`, `capabilities`, and `required`.
Selection requires the same nonzero major, chooses the lower minor, and returns
the sorted capability intersection. Both sides' required capabilities must be
present. Unknown optional names and additive offer fields are tolerated; malformed,
duplicate, oversized and unoffered required names fail closed. A capability's
semantics remain stable throughout a major; semantic changes require a new name
or major. Minor compatibility is not a grant of a capability or any permission.

The initial advertised capabilities are only:

- `snapshot.v1`: discovery/pairing snapshot
- `pairing.v1`: existing create/join/reload commands, with their current role checks
- `connection-diagnostics.v1`: read-only inspection of a known connection ID

`session-owner.v1`, `settings.v1`, `exit-all.v1`, `portal.v1` and `tray.v1` are NOT
advertised or dispatched. The existing directed-delivery path is unchanged and
is not being generalized into a new capability in this slice.

## Compatibility and trust are distinct

The legacy Snapshot stays schema 2 with exactly the original eight fields. Old
Snapshot/CreateGroup/JoinGroup/Reload/InspectConnection JSON shapes are frozen by
regression tests. No extra field is added to a message that old strict readers
would reject. Negotiation uses a separate request/response variant.

Before negotiation the client still requires the exact executable SHA-256,
package version, schema and profile. The negotiation request/response additionally
binds the current generation and executable hash. Private directory/socket or
named-pipe protections and existing GUI generation verification are unchanged.
Negotiation does not authenticate an arbitrary process or replace these checks.
The hash in a message is not new cryptographic proof of the sender's executable.

Consequently this slice does NOT make independently upgraded or differently signed
service/GUI binaries interoperable. It provides a distinct compatibility contract
while retaining the current identity policy. A future signed-identity policy or
approved executable set needs a separate concrete security review; never remove
the current hash check merely to demonstrate mixed-version operation.

## Session lifetime model only: current GUI close is unchanged

`app/src/service_contract/session.rs` is an executable, side-effect-free contract
for the next ownership migration. It reuses `remote_core::media_demand` rather
than adding another media registry. Attachments include the service generation,
connection ID, channel incarnation and consumer generation.

- GUI detach first emits release of that GUI's exact held input, then removes its
  A/V demand. The authenticated channel remains open.
- Hiding a GUI also releases its input before media activity can stop, including
  when another consumer keeps the aggregate video demand active.
- Remaining consumers determine video/audio independently by OR. File, clipboard
  and control lifetimes remain separate; detach does not silently destroy them.
- Reattach does not replay held keys/buttons or grant input. Existing identity,
  source revision, focus, activity, actual paint and permission checks still apply.
- Explicit close/ExitAll or owner/transport failure releases input, pauses A/V and
  requests channel close. Closing is distinct from confirmed Closed; both forbid
  attach/resume. Repeated close/detach and stale callbacks cannot revive a channel.
- Every new channel incarnation must receive a non-reused scope. The existing
  consumer registry uses checked generation increments and a 64-consumer limit.

Integration MUST use one service actor owning model and channel, drain returned
effects in order before its next model command, and never send unversioned effects
to a replacement channel. ReleaseInput deliberately references an attachment
already revoked by the model: execute teardown against its retained old input
binding, not `accepts_callback` or a new GUI grant. Asynchronous rendering needs
its separate existing generation fence. This model is not an asynchronous effect
queue and does not itself perform releases or transport operations.

No call from the current GUI Drop/close path was changed. `WorkspaceConnection`
and GUI `SessionState` still own outgoing viewer resources; their Drop paths still
send Close/Unsubscribe. This slice therefore is NOT acceptance of “close GUI and
keep its viewer channel alive.”

## Next smallest runtime integration

1. Move one existing viewer channel's owner into the existing service owner, with
   no renderer or network-protocol replacement. Keep GPUI rendering in the GUI.
2. Add a narrowly scoped attachment adapter for that owner using this model. Bind
   all callbacks/effects to its service/channel incarnation and preserve release
   ordering. Detach the GUI consumer while retaining the channel; reattach to the
   same owner without creating a second network runtime.
3. Before that ownership integration, fix the existing frontend epoch gap:
   polling currently compares generation only immediately before/after its own
   request. A pairing command can update the cache between polls and hide that
   transition. Use a revision recorded at every cache transition and test this
   interleaving; do not rely on the current poll comparison for migration safety.
4. Only advertise session ownership after real GUI attach/detach, crash, service
   restart, another active consumer, held input and active file-transfer tests.
5. Settings require typed reads and revision-checked apply against service-owned
   state. GUI drafts remain GUI state. Do not expose arbitrary paths or commands.
6. Tray open/settings actions should activate the verified installed GUI. ExitAll
   requires coordinated GUI/session/service stop plus an explicit decision about
   the existing supervisor's KeepAlive behavior, otherwise the service may restart.
   No kill-by-untrusted-PID API or startup-setting change is introduced here.
7. Portal/capture consent needs its existing platform/session ownership and approved
   user interaction; a service bridge must not grant permissions, bypass TCC or
   replace the established identity. Cross-platform native validation remains due.

## Verification entry and limits

`python3 -B scripts/test_stream_settings_model.py --mode service-contract` adds a
fixed focused test mode to the existing entry. It retains lifecycle preflight,
offline/locked dependencies, existing target, source-change guard, 4 GiB reserve,
and owned-process cancellation. The filter includes the new service contract,
session model, and background_service::service_contract_tests modules.

These tests exercise production negotiation functions, actual Request/Response
serialization and framing, legacy DTO compatibility, identity/generation rejection,
cache concurrency, and the pure model. They do not start a GUI or access a device.
The existing private-socket frontend lifecycle integration test also asserts the
negotiated result, but is a separate integration test, not part of that filter.

A separately authorized environment can supply `--lifecycle-authority`,
`--lifecycle-binding` and `--lifecycle-helper` together. Its metadata must name
its real source and existing target, and must be generated/checked by the verified
original helper. The checker preserves full authority/Cargo digests, exact paths,
false coverage flags and an empty cleanup plan. The runner checks the effective
Cargo target (and reported build directory) against those observed references.
The original/default authority and binding are not rewritten.

That explicit environment uses a project activity flock across pre-observation,
Cargo, owned-child shutdown, source/evidence hash checks and post-observation.
Cargo's own target lock remains in place. SIGINT/SIGTERM cancellation terminates
only this runner's new process group, waits for its parent and handles remaining
owned descendants before unlocking; repeated cancellation cannot interrupt that
teardown. This does not claim coordination with unregistered IDE/cleanup tools;
cleanup and managed helper-run authority remain absent. `build_ready=false` and
all helper coverage flags remain truthful: this is the existing authorized
project-specific runner, not a new managed project or a fabricated adopt-run.

Cloud-only authority/binding/lock files are environment evidence, not product
changes. Keep them out of commits, publication and user-machine synchronization.
The final task receipt separately records preflight, compilation, executed tests,
skips and physical verification. A formatting parse or Python suite is not a Rust
compile/typecheck or a successful socket integration.

Windows, Linux and macOS physical GUI/tray/startup/capture/input tests, signing,
packaging, publication, deployment and operating-system settings are outside this
slice and have not been performed. The earlier cancelled frame-recovery publication
is not retried or included in this independent patch.
