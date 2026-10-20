#!/usr/bin/env bash
# Retired historical split host/client launcher. Never builds, signs or kills a process.
set -eu
cat >&2 <<'GUIDANCE'
This historical host/client launcher is retired.
Weavelo uses the unified original-GPUI product and formal identity-bound producers:
  python3 scripts/build_identity_gate.py build --help
  scripts/package_macos_unified.sh
  python3 scripts/build_windows_current.py --help
  scripts/build_android_release.sh
  python3 scripts/install_linux_desktop.py --help
Existing Weavelo installation: python3 scripts/install_macos_unified.py --help
First legacy-identity migration: python3 scripts/migrate_macos_identity.py --help
These commands require reviewed build/signing expectations. This launcher performs no action.
GUIDANCE
exit 64
