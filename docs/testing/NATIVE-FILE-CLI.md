# Native file commands

The original running GPUI owner handles these commands through its existing private
same-user endpoint. The CLI checks the GUI executable hash and current generation.
No second discovery runtime, transport protocol, screen capture or input grant is
created. Existing pairing, file availability, receive limit and no-overwrite policy
remain in force.

- `weavelo --file-peers-json` lists discovered public device IDs and online state.
- `weavelo --file-send DEVICE_ID /absolute/source --sha256 LOWERCASE_SHA256 --request-id UNIQUE_ID --confirm-send`
  reserves the request before connecting and queues the existing original file-send
  service over a silent file-only connection.
- `weavelo --file-status UNIQUE_ID` returns the durable receipt, connection/transfer
  IDs, byte counts and terminal state. A timeout/lost reply requires querying this
  same ID. Repeating the same bound request returns its receipt without sending it
  again; reusing its ID for different inputs is rejected.

Receipts are private files under the original profile's
`gui-state/file-requests`. They survive owner restart. An incomplete receipt is
preserved and never automatically replayed. Do not choose a new ID merely to bypass
an uncertain result. A stale GUI generation is rejected before dispatch.

The sender's native completed state is distinct from independently checking the
receiver's saved file and SHA-256. Real two-device acceptance records both. A
matching locally prepared payload or SSH copy is not a native transfer receipt.

## Independent preferences and receiving boundaries

The new desktop instance snapshots the complete old parent preferences document
once into `Desktop/preferences.json`, under its own stable file lock. The parent
document is read only. Later parent changes never flow into an existing instance.
Atomic updates merge only changed fields so unknown nested user settings survive.
Fresh profiles use the product defaults. Existing false values are preserved by
migration, and invalid documents fail closed. The scoped setting can be changed
explicitly through the normal private GUI business endpoint; the parent is never
written.

Both passive-host and GUI file receivers use `Desktop/Received Files` by default.
A passive-host `WEAVELO_FILE_RECEIVE_DIR` override and a GUI destination choice must
stay within that same private profile. Linked, foreign or nonprivate directories
are refused without permission repair. The old top-level `receive_dir` value is
retained in the JSON as user data and is not adopted as the new instance's shared
destination. Existing size limits and `allow_overwrite=false` remain unchanged.

File sessions require the existing device-group session-key handshake. The host
does not allow file channels through the plaintext compatibility path, checks the
current group key at command/data ingress, and uses the original authenticated
session table. A display name or claimed device ID is not authorization. Group
key rotation revokes the existing session scope; no file protocol is replaced.
When the local file setting is false, GUI media connections use the existing
media-only path and do not start a file receiver. Explicit receiving consent is
required before changing an existing disabled choice. The user has approved
enabling the new Book instance for the authorized desktop group.

## Numbered receiving, setting changes and recovery

With the default no-overwrite policy, the receiver atomically publishes the
requested filename or `name (1).ext`, `name (2).ext`, etc. Existing files are
retained even when several receives finish concurrently. A partial file is never
published as a completed file. The sender waits for the receiver's acknowledgement
after checksum verification, file sync and directory sync. Native reliable
delivery retries lost manifests, chunks and acknowledgements with the same
object identity; the receiver's current-session completion cache prevents another
numbered copy when the final acknowledgement is lost.

`--file-peers-json` also reports the current scoped preferences SHA-256, stored
file flag and paired-group scope digest (never the group secret). To change only
the new instance, use
`--file-set-enabled true --expected-preferences-sha256 SHA256 --expected-group-scope SHA256 --confirm-change`.
This is a version-checked update under the existing private preference lock and
preserves unknown user fields. A stale version or changed group is refused.
Restart only the new instance normally to apply the passive host's setting.

`--file-resume REQUEST_ID --confirm-resume` uses the original receipt and source
binding. Only a verified `failed_before_send` with zero bytes and no connection
or transfer ID can be admitted again. An existing native transfer is queried
without issuing another send; its normal reliable worker handles chunk retries.
Completed requests return the original receipt. A queued transfer with lost
connection/generation or an unknown result requires receiver verification; it is
never blindly replayed, and restart-spanning partial-file resume is not claimed.
Changing the request ID to bypass this boundary can create duplicates and is not
a recovery procedure.
