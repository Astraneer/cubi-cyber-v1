#!/usr/bin/env bash
# The donor is reference data. Never source or execute its contents.
set -euo pipefail
repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
exec python3 "$repo_root/tools/import_donor.py" "$@"
