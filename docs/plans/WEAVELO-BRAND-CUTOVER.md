# Weavelo current-brand cutover — 2026-10-20

The latest instruction changes current source identifiers, new outputs, active
deployment entries and URLs. Historical commits, published releases, recorded
acceptance evidence and retained rollback material are preserved. No permanent
old-brand runtime alias is the final target. A first-migration tool may still
identify a historical installation until its original transaction is complete.

This inventory is a migration boundary, not permission to change credentials,
DNS/TLS, platform grants or protocol authentication. The repository rename is
owned by the coordinated GitHub operation on repository ID `1232002162`.
Source edits use the original primary `main` directory; no second checkout,
dependency change, product rebuild or Actions run is part of this source pass.

## Completed installation before this pass

The HO5 same-profile update is `committed` in its original installation journal.
Its actual running image is built from
`40c3bbccd6b181009d87244e3eacd7f912cac98b`, SHA-256
`226d23013131b99186590ee8b69e66037ca842c9f3eaf2c30f064f1519532bed`.
The old app inode remains in the original private `previous-app` rollback.
Profile bytes and the main unit are unchanged.

A runtime CPUWeight drop-in changed to `10000` during startup. Its writer is
unproven. An exact reviewed journal reconciliation preserved that value and
reverified the same new owner; it did not repeat service actions or exchange
files. CPU contention performance remains untested.

The adapter and tests were pushed as
`c59dc00f819f5be705d7e478cf11e25287a587df`, after workflow removal. That commit is
source for the installer, not a relabeling of the existing runtime binary.
The same repository is now `KingBright/weavelo`; repository ID and Git history
are unchanged and the primary checkout's origin uses the new URL.

## Current source scan

The initial tracked-source scan covered 1,389 files and found 1,163 case-insensitive
matches in 166 files for spaced, joined, underscore and hyphen variants. It includes
historical review records, so the total is not an active-runtime count. A separate
scan found 40 distinct `RP_`/`RH_` or protocol-marker tokens outside vendor and
dated reviews; those also need classification, not blind substitution.

This pass renames the product-owned Ely profile entry and vendor patch notes,
updates current product/docs identifiers and obsolete runtime-variable examples,
replaces machine-local documentation links with relative links, and retires the
obsolete certificate-creation helper. Current diagnostic, fixture and build
environment switches now use `WEAVELO_` names together with their callers.
Windows production restoration scrubs both current and historical acceptance
switches so an inherited test environment cannot disable normal operation.
Upstream licenses, package identities,
revision pins and recorded upstream hashes are preserved.

The initial and current machine-readable inventories are local evidence under
`.rust-storage/validation-logs/`. They record locations and hashes, not invitation
codes, pairing secrets or signing passwords.

## Remaining current identifiers and coordinated changes

| Surface | Observed source or installation | Required target/action | Boundary |
| --- | --- | --- | --- |
| GitHub | repository ID `1232002162`; canonical slug `KingBright/weavelo` | Same-repository rename completed; primary origin and current README use the canonical name | No new repository or forced push; historical receipts retain old locators |
| Source roots | Studio `/Users/jinliang/workspace/remote_play`, HO5 `/var/home/liang/workspace/remote_play`, cube `C:\Users\Liang\Workspace\remote_play`; MacBook already `/Users/jinliang/weavelo` | Same-directory/inode relocation to sibling `weavelo` through the official lifecycle binding and workspace procedure | Preserve WIP, locks, shared targets, current delivery signatures and historical observations; no manual registry rewrite |
| macOS runtime | Both Macs use `~/Applications/Weavelo.app`, `fun.hackerlife.weavelo`, `fun.hackerlife.weavelo.host` | Verify current signed output and remaining user-facing locators without rotating the pinned signer | OS consent remains normal user consent |
| HO5 runtime | `weavelo.service` enabled/running; old `remote-play-current.service` disabled/dead; old desktop entry and directories remain | Bind old unit/desktop entry to the original migration journal, then retire launchable old entries into the owned private rollback set | Do not delete old profiles or replay completed installation |
| cube runtime | PID `34140` runs `AppData\Local\RemotePlay\remote_play.exe`; old Start Menu shortcut remains | Finish the original signed packaging/identity migration, then retire the exact old executable shortcut/entry | Existing effective PowerShell policy is `Restricted`; package and GUI-gate scripts are unsigned |
| Android runtime | `fun.hackerlife.weavelo` code19 and old `com.remoteplay.client` are both installed | Book's unique executor inventories/preserves data and completes the approved phone migration before retiring the old app | This source task does not transfer secrets, clear data or uninstall either package |
| Android signing | `Library/Application Support/RemotePlay/Signing/remoteplay-release.p12`, alias `remoteplay`, Keychain service `com.remoteplay.android.release.signing` | Review local same-key locator/alias/item migration to Weavelo names, then change producer defaults together | No new key, key export, automatic password copy or certificate rotation |
| Android JNI staging | Explicit old-library exclusion in `android/app/build.gradle.kts` | Retire the owned stale native input and remove the exclusion only after the exact new JNI input set is verified | Do not allow a stale library into the APK or delete unrelated JNI libraries |
| Octendra integration | Portal worker ID `io.github.KingBright.RemoteHosts.DesktopWorker` in `host/src/linux_portal/runtime.rs` and its tests | Coordinate one exact new registered ID with the Octendra writer and its desktop-file basename | Do not guess the ID or expand accepted identities |
| Authentication and group routing | Seven branded KDF/MAC context strings in `remote_core` session/relay/P2P code | Review one coordinated protocol-domain transition across all participating versions | Changing these bytes changes keys/MACs/group capabilities; no silent fallback or weakened checks |
| Protocol abbreviations | `RPM2`, `RPDISC1`, `RPR1` | Preserve existing wire bytes pending a coordinated version transition; same-process diagnostic/fixture switches have been renamed | Existing invitation validation and anti-replay behavior must remain intact |
| Historical installers | Old identity paths, labels, environment scrubbers and tests in first-migration tools | Keep only while an original device transaction requires them; retire after all target migrations and rollback ownership are established | They identify historical preimages, not a permanent runtime compatibility promise |

## URLs and infrastructure

| Existing endpoint/reference | Status and required decision |
| --- | --- |
| `https://github.com/KingBright/weavelo.git` | Canonical URL after the same-repository rename; historical evidence stays unchanged |
| `wss://relay.hackerlife.fun:8443/v1/relay` | Current source endpoint. No replacement DNS name has been selected or authorized here |
| `https://relay.hackerlife.fun:8443/download/` | Current publisher base. New source defaults use Weavelo artifact names; NAS actual aliases, Caddy and manifests require the NAS owner's inventory |
| `p.hackerlife.fun:3478/udp` | Current rendezvous endpoint. No DNS or public-route mutation performed |
| Historical `download/RemotePlay-Android-latest.apk` | Found in old deployment records. Verify whether it is still an active public alias before a controlled retirement; do not rewrite immutable release history |
| `/opt/weavelo-relay`, `weavelo-relay.service`, `/opt/weavelo/downloads` | Current deployment declarations already use Weavelo. They are source declarations, not proof of the NAS's current runtime |

New DNS names, certificate SANs/TLS termination, OAuth callbacks and changes to
credentials or permissions must be returned as exact targets with effects for
central review. No domain is inferred, purchased or changed. No OAuth configuration
was discovered in this scoped source pass; that does not prove external account
configuration is absent.

## Explicit exclusions and acceptance

`/Applications/RemotePlay.app` on MacBook has Bundle ID
`com.playstation.RemotePlay`; Android also has `com.playstation.remoteplay`.
These are third-party PlayStation products and are outside this migration.
An old-name substring alone never establishes ownership.

Dated reviews, old build receipts/manifests, cryptographic hashes, release history,
private rollback files and the original Git history are not rewritten. The ignore
rule protecting historical private provisioning data also remains until its
ownership is handled; removing an ignore rule must not expose that data to Git.
The historical DNS helper remains byte-for-byte pinned to its recorded bootstrap;
its generic product-name comment is not an active runtime alias.

Current automatic verification: the release suite ran 633 tests in 15.539 seconds,
`OK (skipped=10)` on Studio; locked/offline Cargo metadata resolves the renamed profile entry with
unchanged source/lock bytes; the retired signing helper exits 64 without performing
an action. No product build or new package was produced by this brand pass.

HO5 control/discovery relay handshakes pass, but accepted peers are zero.
Real user QR joining, actual mutual visibility, capture, video, input, audio and
long-duration performance remain separate unpassed acceptance. Running services
and successful code tests are not evidence of those behaviors.

## Reviewed route fix and deployment follow-up

The independently supplied route patch, SHA-256
`09ed336200fdb43bc3aefacdce57c1a913af5ed8f404259e0cf9c738982bf9d4`,
is integrated in `desktop/model.rs`. Endpoint deduplication now retains the first
preferred route even when duplicates are separated by priority sorting. Five
tests compiled from the actual function and test module pass. This fixes bounded
candidate attempts after discovery; it does not explain an empty discovery list.

The separate computer-use review patch, SHA-256
`1450d9c95da8984fc57cb50f128a3fee707da1f0c257b57fe3e826f225483c3c`,
is preserved in the ignored local review evidence. It has not been integrated.
Its owner-local adapter is limited to existing visible authorized GUI sessions;
Octendra authenticated dispatch and GPU frame export remain separate work.
Weavelo ADB work was cancelled; that capability belongs to Octendra.

The latest runtime sample has Studio on `db9fc29` with a build patch and HO5 on
`40c3bbc`. Studio lacks the GUI network diagnostic command. HO5's relay is
connected/active but has received no discovery frames; UDP has accepted only its
own traffic for rejection. The HO5 background-status mismatch is a separate
unresolved observation. Neither UI filtering nor duplicate device IDs is proven.

Before the next build, the official lifecycle observation refreshed only the
current lockfile digest in its existing binding; cache paths, configuration,
authority and budgets are unchanged. Studio's fixed macOS signing preflight
currently fails because the pinned private signing identity is unavailable;
MacBook is offline in Octendra. No replacement key or unsigned installation is
permitted by that failure. A compiler result alone will not close deployment.
