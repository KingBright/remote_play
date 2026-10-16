# Weavelo identity migration — 2026-10-16

User direction: use Weavelo / weavelo / fun.hackerlife.weavelo and the final V7 half-knot W assets. One primary main checkout; no legacy compatibility layer. Preserve backups, encryption, authentication, platform consent, existing signing keys and unrelated WIP.

## Source changes

Product crate/binary: weavelo_app / weavelo. Native Android library: weavelo_android. Android package, JNI names and desktop application IDs: fun.hackerlife.weavelo. Runtime environment names, profile defaults, IPC pipe names, QR scheme, service labels, packaging manifests and UI use Weavelo. Own internal generic core/platform crate names stay unchanged. Asset provenance is assets/brand/weavelo/SOURCE.json. No other product assets are imported.

macOS packaging includes AppIcon.icns before signing. The source policy retains certificate 8eaa97365d97b8a987cdf1232430eddf9aba8ecf and binds its designated requirement to the new Bundle ID. Windows MSVC uses existing SDK rc.exe to embed icon resource 1, matching GPUI's existing loader. Windows process AppUserModelID and Linux window/desktop IDs use the same reverse-domain identity. Android has exact legacy-density icons, adaptive layers for API 26 and monochrome layers for API 33. Web has local favicon, touch icons and manifest; no DNS or hosting mutation.

## Explicitly preserved security material

Cryptographic KDF/MAC domain bytes remain unchanged: these are protocol identifiers, not display names. No old package/scheme/environment fallback is added. A protocol-domain rename would need a separately reviewed coordinated protocol version transition.

Android signing script still locates the existing physical signing directory, keystore alias and Keychain service. The new environment variable names do not create or duplicate credentials. The setup script is not run during migration. Existing keys are not copied, printed, rotated or exported.

## Required migration transaction before installation

1. Record existing checkout HEAD/main, two protected WIP hashes, complete status, old signed product identities, current service process identities and private rollback paths. Stop only owned builders after observing completion.
2. Rename /Users/jinliang/remote_play to /Users/jinliang/weavelo once. Preserve the same .git and working files. Update lifecycle root/authority references by a reviewed metadata migration, preserve ~/rust-target and all budgets, then refresh the observation binding. Rebind the existing managed workspace; do not create a clone/worktree or symlink alias. Origin remains https://github.com/KingBright/remote_play.git until a separate repository-host rename is completed.
3. Build and sign new brand artifacts through formal producers; do not rename an old signed package or overwrite the old alpha9 evidence. Run identity, authentication, replay, JNI/resource and platform package checks.
4. Plan an explicit old-to-new identity migration. Existing compatible-upgrade installers do not establish that authorization by themselves. Freeze/stop the old service before starting a new network owner. Preserve private old data and verified rollback binaries. Prefer new pairing under the new profile; optional data/settings import must be explicit, local, bounded and permission-preserving. No secret export.
5. macOS: new Weavelo.app / Bundle ID / launch label / profile, same pinned certificate. Old TCC grants may not apply. Ask the user through normal OS UI for necessary consent; never edit/reset TCC. Android: new package installation and runtime consent, same existing release signer. Windows: new product directory/shortcut identity/profile and own launcher; no ACL/policy edits. Linux: new weavelo.service, desktop and private profile; actual Portal consent remains a user-session step.
6. Verify actual running image/version, no duplicate old network owner, signed identity, icons, platform permission handling, real capture/decode/presentation/input/reconnect and preserved security. Keep deployment and physical acceptance separate from compilation.

## Gates still open

The directory and lifecycle authority migration, old-service cutover, new persistent identity installation and any credential-store locator migration require the parent-reviewed concrete transaction. They have not run. Windows temporary RemoteSigned is still awaiting specific human approval; no policy command or Bypass is executed. No NAS/BW/DNS/GitHub repository settings are changed. OAuth/entitlement configuration was not found in the checked first-party source or the current Mac app.

## Implemented first-migration entry (not applied)

`migrate_macos_identity.py` is separate from `install_macos_unified.py`. The latter still refuses a different Bundle ID/certificate/designated requirement and still requires an existing Weavelo app. No compatible-upgrade check is relaxed.

The first-migration entry requires independently supplied new commit/identity/manifest/observer hashes and the exact old executable hash. With no action flag it only reads and verifies the signed package, old signed app, actual native owner, both old/new system-service absence and canonical user paths. It returns a complete plan and SHA256. That plan includes old PID, inventories and hashes, desired new service policy, private backups and optional preferences import. An existing/new/unknown transaction blocks a second apply.

For the final approved transaction, repeat the same arguments and append `--apply --plan-sha256 REVIEWED_PLAN_SHA256`. `--import-preferences` must be selected at both planning and apply to perform a one-time local preferences import. It never imports old pairing keys; the new backend creates a fresh authenticated profile. Preference files are outside NativeMesh: the old parent preferences.json is backed up even when import is not selected. Neither the old profile nor old release-state file is modified.

The transaction verifies the staged candidate before stopping the precise old launch job. It waits for the old job/PID/executable to disappear and holds the old profile lock before moving the old signed app and launch plist to the private fixed backup. It then installs the new app/profile/agent/release state, starts only the new backend, verifies its authenticated status and native mapped image, and verifies the old owner remains absent. No GUI or TCC prompt is auto-started. On ordinary failure it attempts rollback; on interruption retain the original journal and call `--recover --plan-sha256 ORIGINAL_REVIEWED_SHA256` with the same independent artifact expectations. For a completed transaction use explicit `--rollback`, not replay/recover. Failed new app/profile/preferences remain private and recoverable. Concurrent edits or unverifiable owners block recovery rather than overwrite unknown state.

The old start.sh is retired: every invocation exits 64 with formal producer/migration guidance. It performs no compilation, ad-hoc signing, certificate setup, pkill or launch.

## Linux desktop/icon transaction (not applied)

`install_linux_desktop.py` requires an already installed, independently verified Linux release and the pinned SOURCE.json asset manifest. It binds the absolute daily executable hash, user data directory, preimage/mode and new bytes for all nine files: desktop entry; 16/32/48/64/128/256/512 PNGs; scalable SVG. All use fun.hackerlife.weavelo. The desktop Exec is quoted and absolute; unsupported control/quoting characters fail closed. XDG_DATA_HOME is honored only within the authorized home, or selected explicitly with --data-home.

Plan first. Final apply requires --apply plus the exact plan hash. Existing files are privately backed up before any replacement; atomic writes preserve recoverable preimages and modes. Failed/interrupted transactions use --recover and the original plan hash; completed installations can use explicit --rollback. Unknown user edits block recovery. This entry does not control Linux services, rewrite global icon caches, install packages or claim actual system icon display. The existing HO5 service upgrade journal remains unapplied and must be superseded by an independently reviewed new-brand service cutover.

## Exact source-directory rename proposal (not executed)

1. Freeze source edits/owned producers briefly. Record main HEAD, full status, git directory identity, root inode/device, all affected metadata bytes and SHA256, active journals and held locks. Refuse an active/unknown producer. Do not stop the separately installed RemotePlay service merely to rename its source directory.
2. Require /Users/jinliang/weavelo to be absent and on the same parent filesystem. Rename the existing /Users/jinliang/remote_play directory once; retain the same .git/main/index, ignored artifacts, untracked logs, lock inodes and every WIP byte. No clone, worktree, symlink alias or cache move.
3. Apply only the separately reviewed compare-and-swap metadata patch: docs/plans/ARTIFACT-LIFECYCLE-ADOPTION.json root-prefixed paths, product label and app package name; .rust-storage/adoption.json root/authority locator and refreshed authority digest; .rust-storage/release-binding.json authority/declaration paths; .rust-storage/release-deliveries.json root/state/output references. Preserve project_id, budget/reserve/ownership classifications, false coverage, unknown/protected references and existing current-component records. Old signed artifact paths and receipts are not renamed or relabeled as Weavelo.
4. Refresh only the current Weavelo observation binding with the official helper against the moved authority and the same /Users/jinliang/rust-target cache. Preserve original alpha9 observations as historical evidence. Run observation/adopt-check and cargo metadata; verify target/build directories remain the original cache. Do not repair the protected WIP by resetting it to Git.
5. Rebind the existing authorized MacBook project/workspace to the new path without changing device permissions or selecting another execution environment. If the app requires a new path grant, pause that rebind for the owner; do not create a cloud task or broaden device authority.
6. Verify the same HEAD/main/git directory and complete status after the move. Unrelated WIP must remain unstaged; compare its content excluding only the exact approved metadata-field edits. The GitHub origin remains unchanged until a separate repository-host rename is authorized and completed.
7. Before a new-brand release, reconcile its new default artifact names with the existing public current-delivery registry through a separately reviewed exact transaction. Preserve the old 9fe package/validation references as verified rollback evidence. Do not clear registry state or silently rewrite old validation hashes to admit a new name.
8. If metadata/workspace verification fails before any new producer, reverse the same directory rename and restore only the transaction's expected metadata postimages from recorded preimages. Unknown concurrent edits or new active producers block automatic rollback; preserve both the journal and original metadata backups.

Actual directory/authority migration, service installation, new persistent identities and necessary OS consent remain final reviewed actions. GitHub push approval is independent; no push retry or alternate route is used.

The existing macOS registry currently binds component `macos-arm64-release` to the RemotePlay archive/receipt names. A Weavelo filename under that same component will be rejected by the public helper with `current default path/content changed; preserve it`. Directory relocation alone does not resolve this. Do not clear the record, relabel its signatures, or bypass this check: the first new-brand delivery needs an explicit exact authority/retention transition that keeps the old signed pair recoverable. This is an outstanding production-entry gate, not a failed source check.
