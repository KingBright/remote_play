# User profile ownership: prevention and device checks

The current user installer already rejects real UID 0 and does not initialize
credentials. The remaining writer was shared runtime initialization:
`AppPrivateMeshConfigStore::load_or_generate` unconditionally called `save` for an
existing valid identity. `save` wrote both private files through temporary-file
rename, which assigns the new inode to the caller. A privileged invocation pointing
at a user's profile could therefore replace both files with root-owned 0600 inodes.
The same pattern is visible at historical source `f4c7212` and pre-fix `b4fadf6`.
The main entry, `--device-group-ensure`, relay and P2P setup all call this shared path.

This identifies a reproducible code path, not the historical operating-system actor.
MacBook's two root-owned files have close 2026-09-30 mtimes, but no audit receipt
identifies the exact invoking script/process. The external system mesh plist exists
as root/0600 and could not be read; that read was not retried. A separate fixed
`launchctl print system/com.remoteplay.mesh` returned the documented not-found
response, exit 113. No system service was stopped, started or modified.

## Resulting behavior

- A valid existing profile is read-only during ordinary startup, including supported
  legacy metadata. No automatic normalization/re-save changes bytes, inode, mode,
  ownership, mtime or ACLs. Explicit save retains metadata canonicalization.
- Actual effective UID and filesystem metadata guard the directory and both files
  before reading/changing credentials. Root cannot use a profile beneath another
  user's directory, including a pre-created root-owned leaf. A foreign owner, user
  symlink, profile leaf link, parent traversal, multiple file links, non-regular file
  or non-private protection produces an actionable error; nothing is auto-repaired.
- Root-controlled OS ancestry links such as macOS `/var` and Android storage prefixes
  are distinguished from profile/user-created links. Leaf reads use `O_NOFOLLOW`,
  regular-file/owner checks and inode checks. This is not a claim about adversarial
  replacement of an entire ancestor directory by another process of the same user.
- New files are private from creation, with exclusive temporary files. Initial
  publication does not overwrite a concurrently appearing identity. Existing tighter
  file modes remain tighter on an explicit save; existing directory protection is
  never changed. Startup with an orphan secret fails while retaining that secret.
- The installer rejects effective root as well as real root. Its preflight inspects
  profile metadata only, before app/service mutation; it neither initializes nor
  repairs user identities. Existing signing and upgrade transaction gates remain.

Only the already locked libc 0.2.186 dependency's target condition expands from
Android to Unix for effective-UID and no-follow APIs. No package/version or lockfile
change, authentication migration, new GUI engine or new configuration format occurs.
Unix owner validation and macOS installer tests do not certify Windows ACL behavior.

## Isolated regression evidence

The [compact regression receipt](evidence/profile-owner-regressions.json) records
exact source hashes, commands, exits and output summaries. All fixtures live in
fresh temporary directories. Root/foreign identities are simulated in test-only
caller metadata; no test runs sudo, setuid, chown, the real app, GUI, network or TCC.

```bash
cargo test --locked --offline -p remote_core --lib mesh::tests
cargo check --locked --offline -p remote_play_app --bin remote_play
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts/tests -v
```

Final Rust result: 20 passed, including nine new ownership/preservation cases.
Python result: 168 run, 162 passed, six native-signing integration cases skipped;
nine new installer fixtures check missing-profile planning, real/effective root,
unreadable/root-owned metadata, unchanged files, links, private modes and traversal.
The main check exits 0. Existing shared target was reused with a 4 GiB reserve;
source bytes remained stable during the final Cargo runs. No packaging/deployment
or platform/native-media acceptance follows from these checks.

## Current real-device access

The [metadata-only device receipt](evidence/device-profile-access.json) keeps
configuration access separate from capture/input consent:

| Device | Actual user/process and profile access | Current boundary |
| --- | --- | --- |
| MacBook | UID/EUID 501. User repaired `mesh.conf` to 501:20, still 0600; inode/size/mtime unchanged and read/write access true. Directory is 501:20/0700. | `mesh.secret` is still root:staff/0600, unreadable. No known-failing launch or sudo retry after this observation. Separate targeted owner repair awaits administrator authentication. |
| Mac Studio | PID 23896 UID 501; configured NativeMesh directory 501:20/0700, both files 501:20/0600; effective read/write checks true. | No owner problem found. Actual product screen-capture/accessibility consent remains unconfirmed. |
| HO5 | PID 1520582 UID 1000; explicit `/home/liang/.config/remote-play-current/mesh` directory 1000:1000/0700, both files 1000:1000/0600; effective read/write checks true. | Active Wayland session and configured artifact verified separately; actual capture/portal/input consent and presentation unconfirmed. |
| cube | RemoteHosts offline in fresh inventory. | Running account/SID and ACL cannot be inspected. Do not apply Unix UID/chown values. |
| Android | RemoteHosts offline; previous USB unauthorized blocker retained. | App-private UID/access and MediaProjection/input consent unverified. No USB/pairing/permission retry. |

Owner/mode plus `os.access` establish current filesystem access under the observed
same UID as the product; extended ACL entries were not separately enumerated.
These do not establish macOS recording/accessibility, Wayland portal capture,
Windows OS input or Android projection/input permission. No other device received
chown, chmod, an ACL edit or consent changes.

The exact remaining MacBook action is the administrator's single-file owner repair:

```bash
sudo chown 501:20 '/Users/jinliang/Library/Application Support/RemotePlay/NativeMesh/mesh.secret'
```

Retain 0600 and the existing material; never recreate/move/read the secret through
tools. After confirmation, inspect metadata and resume one bounded canonical
installed-app start. Its build remains 20260930.7 until an actual compatible
deployment receipt exists. Both Mac directions and Mac↔HO5 remain `not_tested`.
