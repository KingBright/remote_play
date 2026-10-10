# Build, package, installation and running-image identity gate

This independent tooling adds a fail-closed identity chain for the original GPUI product. It does not modify the GUI, background lifecycle, installer, signing policy, deployment entry or installed application. Existing functional, native presentation, input, audio, visual and duration acceptance remain separate requirements.

## Status and ownership

The Mac lifecycle integration is owned by its existing task. Its supplied implementation commit is `69b7a5f26b567af27cd52f2625f989ae8e2c9151`; this tooling has not applied that patch or installed its executable. Integrate only after the owner publishes the reviewed commit and confirms the shared-worktree boundary. The current product and existing candidates do not yet consume the generated build-identity module. The gate deliberately rejects those candidates; it cannot retrofit a receipt onto an old binary.

Files in this change are `scripts/build_identity_gate.py`, `scripts/native_process_identity.c`, `scripts/windows_process_identity.py`, this document and the identity regression tests. The authoritative production routes remain `scripts/package_macos_unified.sh`, `scripts/publish_desktop_nas.sh` and `scripts/install_macos_unified.py`. No signature policy override or ad-hoc fallback is introduced.

## Evidence chain

1. Review the full expected Git commit and actual source inventory. A dirty or untracked implementation must have an independently approved exact patch digest and snapshot digest. Git state alone is insufficient because normalized working-file bytes can differ. Private keys, persistent mesh configuration and generated artifacts are rejected before their contents are hashed.
2. Prepare a typed build configuration: target, architecture, version, profile, fixed native features, original GPUI entry, compiler-version output digest and selected environment digest. macOS additionally requires the separated `--gui` frontend and `--background-service` backend metadata.
3. Run the fixed observed-build wrapper. It uses an explicitly provisioned, nonexistent target directory outside source, pinned/locked offline dependencies, two jobs and disabled incremental compilation. It refuses existing caches and retains a four GiB disk reserve. No full application build was run for this change.
4. Compile the exact identity bytes into the executable and compare `--product-info-json` with the independently prepared identity. Verify source, generated record, compiler environment and toolchain again after linkage. A successful compiler exit without the exact compiled record cannot mint a receipt.
5. On the authorized signing Mac, pass the existing pinned signer guard and seal the final signed bytes. The release manifest is outside the signed `.app`; its digest and receipt travel through an independently verified handoff. Changing a signed resource after signing or claiming a source SHA in a manually written sidecar is insufficient.
6. Verify every package file against that separately pinned manifest. Archive verification streams ZIP/TAR bytes without extracting or launching them. It rejects missing manifests, extra/missing files, traversal, aliases, links, special files and hash mismatches.
7. Verify the installed canonical executable, full installed inventory, compiled identity, native signer where applicable, and independently observed daily launch executable/arguments. Supplying a guessed launch path is not evidence of launcher configuration.
8. Verify the actual running process using native evidence. Linux binds the kernel-loaded inode and image digest through `/proc`; macOS binds mapped executable vnode, process start time and unchanged on-disk identity through the independently hashed observer. Windows compares the main module's dedicated immutable public identity section and process creation/path evidence. These observations are distinct from compilation, GUI submission and presentation.

Every gate result keeps `release_authorized: false`. A self-digest is an integrity check, not independent attestation. Source pre/post snapshots cannot prove the absence of transient source changes during a malicious build. Compiler version output is a configuration fingerprint, not a compiler trust attestation. Receipts are meaningful only when the builder/signer execution and externally supplied expectations are independently trusted.

## CLI and integration interface

Use resolved absolute paths for artifact directories and files. The CLI rejects symlinked path components; platforms with a temporary-directory alias must resolve that alias first. Output files are created exclusively and never overwrite an existing versioned receipt.

```sh
python3 scripts/build_identity_gate.py inspect-source \
  --repo "$REPO" --expected-commit "$COMMIT"

python3 scripts/build_identity_gate.py configuration \
  --platform macos --architecture aarch64 --version "$VERSION" \
  --target aarch64-apple-darwin --profile release --output "$CONFIG"

python3 scripts/build_identity_gate.py prepare \
  --repo "$REPO" --expected-commit "$COMMIT" \
  --expected-snapshot-sha256 "$REVIEWED_SNAPSHOT_SHA" \
  --configuration "$CONFIG" --output "$IDENTITY"

python3 scripts/build_identity_gate.py build \
  --repo "$REPO" --identity "$IDENTITY" --identity-sha256 "$IDENTITY_FILE_SHA" \
  --target "$FRESH_TARGET" --output "$BUILD_RECEIPT"

python3 scripts/build_identity_gate.py seal \
  --stage "$STAGE" --build-receipt "$BUILD_RECEIPT" \
  --build-receipt-sha256 "$BUILD_RECEIPT_SHA" --compiler-binary "$RAW_COMPILER_BINARY" \
  --expected-commit "$COMMIT" --expected-identity-sha256 "$BUILD_IDENTITY_SHA"

python3 scripts/build_identity_gate.py package \
  --archive "$PACKAGE" --expected-manifest-sha256 "$MANIFEST_SHA" \
  --expected-commit "$COMMIT" --expected-identity-sha256 "$BUILD_IDENTITY_SHA"
```

A dirty build additionally requires `prepare --expected-patch-sha256` from a separately reviewed snapshot. The identity-file SHA hashes the prepared JSON bytes; `identity_sha256` inside that JSON binds its typed source/configuration content. Do not confuse these digests.

The wrapper supplies `REMOTEPLAY_BUILD_IDENTITY_FILE` and `REMOTEPLAY_BUILD_IDENTITY_RS`. The latter is a generated Rust module with `BUILD_IDENTITY_BYTES` and `build_identity_bytes()`. The product owner must include it at compile time, parse those exact immutable bytes into ProductInfo's `build_identity`, and rebuild. The static uses `.rpbuild` on Windows and `__TEXT,__rpbuild` on macOS. It must be retained by the actual metadata reference. Missing integration must fail release checks rather than silently claim a clean source identity.

`installed` and `running` require `--app`, `--manifest`, all three independent expected digests/commit, `--canonical-executable` and the actual observed `--launch-executable`. For the macOS frontend pass `--launch-argument=--gui`; the backend process is separately owned and must not be mislabeled as GUI acceptance. `running` additionally takes `--pid`, and macOS requires `--observer` plus its independently checked `--observer-sha256`.

Compile the reviewed macOS observer independently with `cc -std=c11 -Wall -Wextra -Werror scripts/native_process_identity.c -o OBSERVER -lproc`. It does not launch the product, signal existing processes, read process memory, access TCC or enable privileges. Windows uses ordinary query/read rights and reads only bounded PE headers and the dedicated public identity section, never heap/profile/credential contents. Its native API interfaces are [QueryFullProcessImageNameW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-queryfullprocessimagenamew), [GetProcessTimes](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getprocesstimes) and [ReadProcessMemory](https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-readprocessmemory), subject to [normal process access rights](https://learn.microsoft.com/en-us/windows/win32/procthread/process-security-and-access-rights). Access denial fails the gate; it never falls back to argv or an unverified process metadata file.

## Remaining coordinated work

- The product owner must embed the generated immutable module and require compiled build identity in release paths.
- The production packager must consume the observed-build receipt, preserve the raw-to-signed handoff and archive the external manifest. Existing Mac archive guards currently need an explicit reviewed allowance for this new root manifest; this change does not modify or bypass them.
- The installer owner must preserve the manifest and independent expectation transactionally alongside the stable canonical app, maintain the existing old/new signature comparison and verified rollback, and record the actual launcher and running process. Checking a guessed path alone cannot close this boundary.
- Keep per-profile runtime-owner mismatch rejection. A CLI assertion does not replace the authenticated lifecycle owner's observation.
- Native Windows execution and Linux process fixture checks require their actual platforms. Full native visuals, video presentation, input, audio and long-duration acceptance remain pending. No staging package may become the daily application solely because this tooling passes.
- Raw command receipts and host paths belong in private evidence storage. This Git document contains only public source/configuration contracts and sanitized acceptance facts; it provides no public download of raw evidence.

## Validation

Run the required release suite with `python3 -m unittest discover -s scripts/tests -v`. The identity tests use disposable repositories and package fixtures, plus a locally compiled non-GUI child for macOS mapped-image checks. Only that owned child is terminated. Linux's loaded-image fixture is skipped on macOS; Windows PE parsing tests do not count as native Windows process acceptance. No test resets permissions, launches a fixture App or modifies an installed product.

On 2026-10-10 the required release suite completed with **237 tests: 230 passed, seven skipped, zero failures**. The new identity module contributes 40 tests (39 passed, its Linux process fixture skipped on macOS). Native signing integration was disabled. The actual owned macOS non-GUI mapped-image fixture passed and rejected a file changed after process start. A separate tiny Rust CLI compiled the generated identity module, round-tripped its bytes and exposed the expected macOS section; this is module compatibility evidence, not an application build or product acceptance. No product App was launched or installed.
