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
