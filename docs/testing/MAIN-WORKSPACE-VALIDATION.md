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
tests. `computer-use-pixels` runs the bounded CPU RGBA layout/byte tests in the
actual client crate. Export revalidates the session before selecting bytes;
native buffers report `NativeReadbackUnsupported` and CPU color metadata remains
`Unspecified`. `platform-macos-check` also checks the product binary with native video.
These checks do not establish Octendra dispatcher integration, GPU pixel export,
physical presentation, remote execution acknowledgement or device acceptance.

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
