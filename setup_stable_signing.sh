#!/usr/bin/env bash
# Retired: the formal producer verifies the existing pinned signing identity.
set -eu
cat >&2 <<'GUIDANCE'
This certificate-creation helper is retired.
Weavelo uses deploy/macos/signing-policy.json and scripts/package_macos_unified.sh.
Use the existing authorized signer; a brand change never creates a replacement key.
Review docs/MACOS_RELEASE.md before packaging or an identity migration.
This entry performs no key, Keychain, trust, permission, build or installation action.
GUIDANCE
exit 64
