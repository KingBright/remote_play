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
