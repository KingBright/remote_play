#!/usr/bin/env bash
# Reuse an independently observed native ELF; never compile or install here.
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec python3 -B "$ROOT_DIR/scripts/package_linux_desktop.py" "$@"
