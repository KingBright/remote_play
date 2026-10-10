# macOS releases: stable identity and verified upgrades

## Why this is mandatory

The September 29 incident was a release identity regression: the user had already granted Screen Recording to the certificate-signed RemotePlay app, but a build from a machine without that private key silently fell back to ad-hoc signing. Same name/path/bundle ID did not preserve the old authorization. Changing TCC settings was not the appropriate first remedy.

Policy contains only the public certificate fingerprint, never a private key. A certificate being unavailable on a worker is a build failure, not permission to create a different identity. Sign on the existing authorized machine after workers compile. Private keys remain in their existing Keychain.

## Supported workflow

1. Keep `VERSION` and Cargo package versions consistent. Build and package with `scripts/package_macos_unified.sh`. The signer is checked before the build; identity, resource integrity and the re-extracted archive are verified afterward. Output filenames include version/build and cannot be overwritten.
2. Fill the ordinary desktop release metadata and publish with `scripts/publish_desktop_nas.sh ARTIFACT_DIR`. The actual macOS archive must satisfy the pinned signer and stable designated requirement before network writes.
3. Plan an upgrade on the target Mac with `python3 scripts/install_macos_unified.py PACKAGE.zip`. Add `--apply` only to apply that plan. Only `~/Applications/RemotePlay.app` and its matching existing user agent are eligible. Same version/build with different bytes, downgraded versions, mismatching signers, and unexpected launch paths are refused.
4. The installer verifies a staged copy before stopping RemotePlay, preserves the old app, installs in place, re-verifies, restarts only the matching agent when it was loaded, and checks stable process state plus unchanged network/launch configuration. Verification/startup/health failure restores the prior app. Identical repeated installs are no-ops.
5. After installation, verify source-list authorization through the running host. Then verify real frame receipt/rendering and input with user-approved capture. Never infer all of these from `codesign` or `launchctl active`.

## Permission recovery

If the checkbox is on but capture fails, record the actual launch path, bundle version, code signature and host error. First compare identity against policy and the previously authorized app. Do not blindly reset ScreenCapture/Microphone/all TCC entries; do not disable SIP, Gatekeeper or unrelated services. Unknown/ad-hoc legacy installations are refused by the routine installer and require separately reviewed recovery to the already-authorized signed app.

## Deliberate limitations

The existing local certificate is not Developer ID notarization. The guard reports signature identity only, not Apple notarization or granted runtime privileges. Moving to Developer ID, changing certificates/bundle IDs, moving the executable, OS privacy changes, or user revocation require explicit review and may require fresh user consent. Administrative manual copies outside these entry points are not protected by this workflow.

References: Apple TN3127 (Inside Code Signing: Requirements), TN2206 (macOS Code Signing In Depth), and ScreenCaptureKit documentation.
