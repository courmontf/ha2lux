#!/usr/bin/env bash
# À lancer une fois après un « git clone » : active les hooks versionnés dans .githooks/.
set -euo pipefail
cd "$(dirname "$0")/.."
chmod +x .githooks/* scripts/*.sh
git config core.hooksPath .githooks
echo "hooks activés (core.hooksPath = .githooks)"
