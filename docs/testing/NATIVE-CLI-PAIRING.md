# Native CLI pairing

The CLI attaches to an existing native owner through the same same-user,
executable, protocol, version, capability-negotiation and session-idle checks as
the GUI. It does not start another owner, edit profile files, approve media
permissions, or bypass a required confirmation.

* `--pairing-status-json` returns public metadata fingerprints only.
* `--pairing-create-group --confirm-change` performs the existing new-group action.
* `--pairing-join-stdin --confirm-change` consumes one invitation from a private
  inherited pipe and performs the existing invitation join.
* `--pairing-export-invite-pipe N` writes the existing invitation to a separately
  inherited anonymous pipe: N is a file descriptor on Unix or a HANDLE on Windows.

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
