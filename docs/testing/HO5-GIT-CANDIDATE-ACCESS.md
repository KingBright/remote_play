# HO5 candidate Git access

Repository: `https://github.com/KingBright/remote_play.git`
Branch: `codex/gpui-mvvm-migration`
Validated binary source: `c4d7031e29e5aa11d18c92e8954603debd394194`.

The current candidate and previous source working directory are the same existing
checkout. The old ref `main@969ca10922eb17e705877b7ca4cf5ce3bec836c1` is retained.
No repository was copied or cloned. The owner handoff contains the exact local
directory; Git documentation omits account-specific paths. Use project-relative
`target/` for the existing build cache. SDK/evidence staging and installed
version directories are separate from source and must not be overwritten.

## Guarded fetch

Run while no other owner is editing/building the checkout. Set
`rp_checkout` to the existing source directory and `rp_expected_commit` to the
full verified branch SHA from the owner handoff. A later documentation commit
does not change the binary's validated c4d7031 provenance.

```sh
set -eu
: "${rp_checkout:?Set the existing checkout directory from the owner handoff}"
: "${rp_expected_commit:?Set the full verified branch SHA from the owner handoff}"
cd "$rp_checkout"
test ! -e .git/index.lock
test -z "$(git status --porcelain=v1 -uall)"
test "$(git branch --show-current)" = codex/gpui-mvvm-migration
test "$(git remote get-url origin)" = https://github.com/KingBright/remote_play.git
git fetch --no-tags origin refs/heads/codex/gpui-mvvm-migration
test "$(git rev-parse FETCH_HEAD)" = "$rp_expected_commit"
git merge --ff-only FETCH_HEAD
```

Dirty/untracked files, a different branch/origin, a mismatched fetched commit or
divergent history require inspection. Preserve other owners' work; do not reset,
clean, force-push or copy a new repository. After sync, hash-verify the complete
source manifest. If build inputs changed, refresh only changed verified mtimes
before any concurrent build, following `scripts/refresh_transferred_sources.py`.

Source identity, compilation, native presentation and the installed running
version require separate receipts. Do not install the candidate while visual
acceptance is pending. The earlier paused transfer is not resumed through Git.

Local Python regression: **197 total, 191 passed, 6 skipped, 0 failed**. That
working-tree run includes separate uncommitted Android tests; native signing
integration was disabled. Linux checks and 11 PipeWire tests are recorded
separately in [the validation report](../reviews/2026-10-10/HO5-CANDIDATE-VALIDATION.md).
