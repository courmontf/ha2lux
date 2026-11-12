#!/usr/bin/env bash
# Run once after a `git clone`: enables the versioned hooks in .githooks/.
set -euo pipefail
cd "$(dirname "$0")/.."
chmod +x .githooks/* scripts/*.sh
git config core.hooksPath .githooks
echo "hooks activés (core.hooksPath = .githooks)"
