# Native CLI pairing

The CLI attaches to an existing native owner through the same same-user,
executable, protocol, version, capability-negotiation and session-idle checks as
the GUI. It does not start another owner, edit profile files, approve media
permissions, or bypass a required confirmation.

* `--pairing-status-json` returns public metadata fingerprints and the local device name.
* `--pairing-set-name NAME --expected-name CURRENT --confirm-change` updates
  only the current device label through the existing idle owner and refreshes
  discovery/relay announcements. A changed current label is rejected before
  writing; device ID, group credentials and authorization remain unchanged.
* `--pairing-create-group --confirm-change` performs the existing new-group action.
* `--pairing-join-stdin --confirm-change` consumes one invitation from a private
  inherited pipe and performs the existing invitation join.
* `--pairing-export-invite-pipe N` writes the existing invitation to a separately
  inherited anonymous pipe: N is a file descriptor on Unix or a HANDLE on Windows.

Fresh device labels use the readable local hostname, independently of the product
brand. Existing saved labels, including a deliberate custom name of Weavelo, are
preserved on startup and reconnect. Old metadata has no label-origin marker, so
it is never bulk-migrated by matching the product name. Correct a known generated
placeholder only through the explicit name command after checking its current
identity and label. Peer announcements and the original device list retain the
remote device's name; a local update cannot rename another device.

The invitation is never accepted in argv or environment variables. Console/file
input and ordinary stdout/stderr output are rejected. The caller must close the
pipe writer after sending the single bounded invitation line. Pipe I/O has a
ten-second bound. No invitation file or new persistent credential is created.

Use a trusted process supervisor to connect the export pipe directly to the
joiner's stdin. The supervisor must not read or log the invitation. For a
cross-device join, use an already authorized product transport with protected
input; these commands do not create an SSH session, public listener, file relay,
or new authorization to reach another device.

Changing groups is deliberate: the confirmation flag expresses the caller's
intent and the native service still refuses a change during an active session.
Use only the intended independent Desktop profile; the CLI never migrates or
imports the retained client's membership.

An accepted operation confirms the native business response, not completed
runtime reload, relay registration, discovery, or transfer. A timeout or rejection
is unconfirmed: inspect status and actual owner state before another mutation.
There is no automatic retry, force mode, or shutdown/restart fallback.

The CLI binary must match the running owner's executable. A new CLI build cannot
control an older installed owner; update the designated independent installation
through the normal signed deployment path or validate with disposable native
test owners. Keep the retained installation and group unchanged.

For an existing independent macOS installation, use
`python3 scripts/install_macos_desktop.py --upgrade` to inspect the current
executable, installation-record and launch-policy digests. Apply with `--apply`,
the signed archive, its original source/identity/manifest/archive pins, and
`--expected-current-executable-sha256`, `--expected-current-install-sha256` and
`--expected-current-plist-sha256`. This upgrades the independent app through
normal GUI/background exit, preserves membership and the exact launch policy,
and verifies the running native pairing CLI. An unfinished transaction requires
inspection with `--upgrade --status`; it is never automatically replayed.
Only a confirmed `staged` transaction, before any quit/stop request, can be
explicitly continued with `--resume-staged --expected-upgrade-sha256 DIGEST`
and the same candidate/current pins. The staged signed bundle is reverified;
changed records, changed candidates and any later phase remain blocked.
