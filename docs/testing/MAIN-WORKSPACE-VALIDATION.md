# Validation from the primary main checkout

All subsequent edits and validation use the primary Weavelo repository on
`main`. The historical task source copies and their Git references are retained
only in recoverable local archives. They are not development or build inputs.

The six differences in the retired build copy were resolved as follows:

| File | Resolution |
| --- | --- |
| `.cargo/config.toml` | Archive the copy-specific `.rust-artifacts` target/build paths. Preserve the primary configuration and its existing shared-cache contract. |
| `.gitignore` | Archive the copy-specific generated-directory exclusions. Those generated paths were not added to the primary checkout. |
| `AGENTS.md` | Carry the required source ownership and lifecycle discipline into the primary instructions; retire the copy-specific helper command. |
| `Cargo.toml` | Archive the copy's dev/verify/release profile overrides. Preserve primary Cargo defaults and product build semantics. |
| `docs/testing/2026-10-10-INPUT-STAGE-MATRIX.json` | Keep the newer primary version; archive the stale copy, including its missing delivery evidence and matrix definition. |
| `docs/testing/2026-10-10-PORTAL-INPUT-STAGE.md` | Keep the newer primary version; archive the stale copy rather than losing delivery and regression records. |

Run the existing focused Rust entry from this checkout:
`python3 -B scripts/test_stream_settings_model.py`. It resolves the source root
from its own location, reports the source directory and Git identity, then uses
the existing Cargo target. Its lifecycle observation must pass before Cargo runs.
The existing 4 GiB emergency reserve, owned-process cancellation on low space,
source-change detection, offline/locked dependency resolution and Cargo target
locking remain in effect. This fixed test runs only `stream_settings::tests`.

The stale adoption digest was refreshed using the installed helper's supported
`adopt --write-binding` operation into a new file, reviewed and then replaced.
Only the recorded `Cargo.lock` digest changed; the lockfile itself, dependencies,
profiles, target configuration and ownership protections did not change.

`python3 -B scripts/check_artifact_lifecycle.py check` remains observation only.
Its `build_ready: false` describes the unintegrated generic lifecycle wrapper,
not a prohibition on this existing project-specific, explicitly authorized test
entry. Do not mark shared-target budgets or cleanup/IDE coverage as implemented.
The shared target stays protected; no cache cleanup is performed by this script.

The retired artifact store preserves its signed unique candidates, logs, registry
and cache in place. Its policy describes a historical environment; no executable
build helper or Cargo source workspace remains there. Moving or deleting these
artifacts requires their own ownership and active-use checks. This retirement did
not rebuild, deploy, push, change device permissions, or clear any shared cache.

For release-script checks in the primary checkout, use
`python3 -B -m unittest discover -s scripts/tests -v`; `-B` preserves existing
user bytecode files. A focused Rust run verifies the primary source entry. Earlier compiled evidence
remains tied to its recorded source hashes, profiles and original build location.

## Owner-local computer-use validation

The owner-local computer-use interface has fixed focused modes `computer-use`
and `computer-use-input`. They compile the actual application library with its
GPUI/Tokio dependencies and run the receipt and existing source/input-boundary
tests. `computer-use-pixels` runs the bounded CPU RGBA tests and, on macOS, actual
Core Video/Core Image readback tests in the client crate. Native fixtures contain
synthetic pixels; they never capture a user's screen. They check full/video range,
top-left row orientation, opaque alpha, missing color metadata and size limits.

The macOS observation path reads the existing decoded 8-bit NV12 CVPixelBuffer.
Core Image converts declared source color to tightly packed sRGB RGBA8 and waits
for native completion before publishing bytes. Missing matrix/primaries/transfer
metadata fails explicitly rather than guessing. Output is bounded by both the
caller and the existing 16 MiB limit. Software decoder buffers retain
`Unspecified` color; other native platforms remain unsupported by this export.
The existing native video presenter is unchanged. Export checks the original
session/fence/TTL both before readback and before returning pixels.

`platform-macos-check` also checks the product binary with native video. Native
fixture success is not live remote-device acceptance or physical presentation.
Octendra authenticated dispatcher integration, remote input acknowledgement and
device acceptance remain separate work.

## Linux X11 test compilation on the existing Mac

`python3 -B scripts/test_stream_settings_model.py --mode linux-x11-check`
uses the same lifecycle check, existing Cargo target, locked offline resolution,
4 GiB reserve monitor and source-change guard. The default invocation continues
running only `stream_settings::tests`.

The X11 mode runs `cargo check -p gpui-ce --lib --tests` for
`x86_64-unknown-linux-gnu`; the dependency's default features include X11.
It uses installed Zig and cargo-zigbuild's compiler argument adapter through
process-local, target-specific CC/CXX/AR settings, with the existing `~/.cache/zig`.
It does not invoke cargo-zigbuild as an alternate build entry, install a compiler,
change global Cargo configuration, create a new target root, or skip lifecycle
validation. Linux native libraries and a suitable cross sysroot may still be
needed; do not substitute macOS libraries or suppress their detection to force
success. A successful check compiles test code but does not execute Linux tests
or establish native GUI/input acceptance.

## Native Linux SDK binding (HO5)

Native Linux product builds use `scripts/build_identity_gate.py` with the
existing primary checkout and `target`; a successful Rust-only test is not a
native-video build. Before creating the `configuration`, verify and bind both
`WEAVELO_VERIFIED_FFMPEG_INCLUDE` and any required `LIBRARY_PATH`. Keep those
same values for `prepare` and `build`. The gate includes both in
`environment_sha256` and rejects changes before entering the build producer.
Do not reuse an unbound configuration or add headers after preparing identity.

On HO5, the already verified FFmpeg 8.1.2 development headers are at
`/var/home/liang/workspace/.rp-linux-native-20261003/deps/ffmpeg-8.1.2`.
This directory is a dependency SDK, not an alternate source/build entry.
Its existing archive `../ffmpeg-8.1.2.tar.xz` has SHA-256
`464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c`.
The header pins are:

| Header below the SDK root | SHA-256 |
| --- | --- |
| `libavcodec/avcodec.h` | `909f5c2fe7caf26601c39cd96acbc6f58f49ec5a78914d2e5b524a3fc2571afc` |
| `libavcodec/version.h` | `61f149cce5f51d2005e584565d2051f11176d6e5d025c97bb0c5dabfb8291ac2` |
| `libavcodec/version_major.h` | `b70daf8ad1fbf8c6d1658b79a223119b7a788c9637e4fb6446e98ba8114bdd58` |
| `libavutil/version.h` | `1de4a0ee20c75f04603ec1799faf9f4e650f790674e2f5ed2fdb1ab735cca75b` |

The existing Opus library used by this native build is
`target/x86_64-unknown-linux-gnu/release/build/audiopus_sys-f3c8cd9f658fe3b4/out/lib64/libopus.a`,
SHA-256 `221ab15de9950ce275dd4cfb339be0b56bbc31457092d55ae0f38a537c4b27de`.
Use its verified absolute parent directory as `LIBRARY_PATH`; do not guess
another Cargo build directory if this file changes or disappears. Revalidate
the SDK and library before preparing new build identity. No dependency download,
new target, global environment change, or system development package is implied.

The 2026-10-23 HO5 build of `26a9c0c` used these bindings with the existing
compiler and cache. An inherited `RUST_LOG` was removed from the build child
only, because the original gate rejects unrecorded Rust overrides; it was not
added to a global configuration or silently accepted by the gate.

The native receipt executes the exact compiled ELF through
`--product-info-json`. It proves compiled identity and native features,
not installation, GUI interaction, device discovery or file transfer.
Formal Linux packaging still uses `scripts/package_linux_desktop.sh` and
the original release authority. Missing process/open-file/launch-reference
coverage remains `unknown`, even when the final archive does not yet exist;
it must not be replaced with a raw ZIP or a direct cache-binary installation.
