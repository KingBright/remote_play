# Codex app update recovery and matrix consistency check

The October 9 recovery check found the completed alpha.8 package operation intact. No build, signing, installation or external operation was replayed. HEAD was `41907af787f2e7243311f403ffee3b18155402cb`, the index was empty, no `index.lock` existed, and all 105 pre-existing dirty paths retained their exact bytes and status.

The prepared package remains `2.0.0-alpha.8 / 20261009.181822`, SHA-256 `441d0d66e3092b7024f6111fdf6596a68f3979a027e80b623f45014a88425838`. Its durable provenance still has SHA-256 `30b0eb23675441b54f5fa4bc7a89ab961c242a70f7ea7524166eb141f8d72082`. Public signature verification passed again for the package, the installed alpha.7 app and the existing alpha.6 rollback app. The prebuild binary backup also matches its receipt. All 596 emitted local dependency file inputs and the native Swift directory files still match the original snapshot manifest. This matrix-validator fix is outside those package inputs; the package has not been changed or rebuilt.

The normal account remains UID/GID `501:20`. NativeMesh is an ordinary `0700` directory owned by that account. `mesh.conf` and `desktop-instance.lock` are ordinary single-link `0600` files owned by that account and accessible to it. `mesh.secret` remains an ordinary single-link `0600` file owned by UID 0, with its prior inode, size and mtime; it is inaccessible to the normal account. The existing administrator-recovery wait is preserved. No private file contents were read and no sudo, ownership repair, installation or app launch was performed.

## Reproduced consistency-check defect

`scripts/check_platform_matrix.py` used debug assertions for its validation. Python `-O` (also enabled by `PYTHONOPTIMIZE`) removed every check. A temporary copy of the existing matrix with only its twelve untested cross-platform results relabeled `passed`, still lacking their physical evidence, was rejected with exit 2 in normal mode but accepted as twelve passes with exit 0 under `-O --require-cross-platform`. The actual matrix was never altered.

The validator now uses explicit exceptions, checks unique direction IDs and known kinds, validates hexadecimal artifact digests and integral decoded-frame counts, and returns structured failure for malformed input. Nine targeted offline tests passed, including subprocess checks in normal and optimized modes. Both modes now reject that inconsistent fixture with exit 2. The actual matrix remains structurally valid with twelve `not_tested` directions, zero physical passes, and the expected strict exit 1 in both modes. Existing passed builds and unrelated regression suites were not rerun.

Static platform review found the current host's macOS-only `window_capture` advertisement consistent with its source enumeration and the matrix's unsupported single-application cases on other producer platforms. This does not establish runtime capture, input permissions, actual presentation, audio or two-device acceptance. No protocol, platform-support or GUI implementation was changed.

This tool remains a supplied-data consistency check, not an independent verifier or release authorization. Signing, compatible installation, actual process/version, capture enumeration, decoded frames, visible presentation, input/audio and physical acceptance remain separate.

Small recovery, pre/post reproduction and test receipts are saved in [app-update-matrix-recovery.json](evidence/app-update-matrix-recovery.json).
