# Validation from the primary main checkout

All subsequent edits and validation use the primary RemotePlay repository on
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

Start lifecycle assessment from this checkout with
`python3 -B scripts/check_artifact_lifecycle.py check`. This command is observation
only: it neither compiles nor cleans. At retirement it refused because the
adoption binding records an older `Cargo.lock` digest. The existing authoritative
inventory also declares `build_ready: false`, shared-cache ownership, and missing
build-lock/closure integration. Reassess the lockfile and integrate a reviewed
primary-checkout build entry before compiling. Do not copy the retired environment's
policy over this binding, silently redirect the shared cache, or recreate a source
mirror to bypass the refusal.

The retired artifact store preserves its signed unique candidates, logs, registry
and cache in place. Its policy describes a historical environment; no executable
build helper or Cargo source workspace remains there. Moving or deleting these
artifacts requires their own ownership and active-use checks. This retirement did
not rebuild, deploy, push, change device permissions, or clear any shared cache.

For release-script checks in the primary checkout, use
`python3 -B -m unittest discover -s scripts/tests -v`; `-B` preserves existing
user bytecode files. No full test run was needed for this documentation and local
source-retirement change. Earlier compiled evidence remains tied to its recorded
source hashes, profiles and original build location.
