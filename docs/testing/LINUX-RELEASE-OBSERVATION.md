# Linux release observation

The independent desktop package continues to use `package_linux_desktop.py`,
the original `release-admit` API, original locks, registry and fixed outputs.
This change adds platform dispatch to the original trusted backend; it does not
create another release authority or change any retention policy.

## Observation contract

`scripts/maintenance/linux_release_activity.py` parses bounded process metadata
and NUL-delimited file observations. Its pure classifier requires stable process
identities; complete command-reference, executable, cwd, file and mapping
coverage; compiler attribution; complete Linux launch-source coverage; and exact
original protocol-lock proofs. Busy, denied, incomplete, stale and malformed
observations never become idle. A control PID only exempts its command reference,
not its files or other scopes.

Production currently collects two fixed `ps` metadata snapshots with PID,
parent PID, UID, start time and comm. It does not read command arguments,
`/proc`, credentials, or run lsof. The current execution grant supplies no
trusted complete file-reference or launch-reference provider. The collector
therefore returns unknown and the original publication gate refuses admission.
Complete synthetic fixture observations are tests, not installable evidence.
There is no snapshot-file option, environment override, or imported idle receipt.

## Trusted-source installation

`install_linux_release_adapter.py` requires the independently pinned original
backend source (SHA256
`175566a4e71592c8b6a37fbbac7ed0f75d8030207f608ec6681e730f6b876aa4`)
and the reviewed new activity-source hash. The exact unchanged baseline is\ntracked at `scripts/maintenance/release_backend/managed_release_retention.py`\nso normal Git delivery preserves its bytes without copying rendered tool output.\nPass that path to `--baseline`; it is a source dependency, not another authority\nor a directly supported cleanup entry. It verifies the existing HO5 helper
and release-module hashes and produces an exact installation plan. Apply also
requires that plan hash and holds the original project producer/global locks.
Only two source files are installed into the original trusted backend directory.
Existing mismatching files and interrupted staging are preserved. The macOS
branch and original protocol validator remain intact. No storage cleanup,
registry edit, permission change, application start or product rebuild occurs.

The source installer is not release verification. After source installation,
normal packaging may be attempted with the already observed binary/receipt pins;
an unknown Linux observation must stop it. Do not run the whole packager as root,
disable activity checks or repeat previously denied process reads.

## Corrected historical diagnosis

The original package operation
`cfacaed9-f2fa-4737-9b66-ac46d83c9ed1` failed at release-admit because the trusted
backend source was missing. The separate compatibility probe
`c4cc210f-4f3a-4ddc-8dea-4e56f51d3c77` counted 91 same-UID processes, 86 observable
and five permission-denied. It did not retain exact failed syscall/path/PID
mapping. Those are separate observations; neither proves a specific OS
permission grant is required by the current packager.

The real native main `90244349` build succeeded in operation
`1a83ff54-5985-4a40-9597-5c37b91a7c69`, using the original cache and verified
FFmpeg/Opus dependencies. Preserve that result. Windows RemoteSigned remains
pending, and Weavelo r1064 / EnGraph r1112 unknown-write barriers remain active.
