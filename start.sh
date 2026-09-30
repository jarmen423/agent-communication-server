#!/usr/bin/env bash
# Deprecated entrypoint — kept so old muscle memory still works.
# Use `make up` (see CONTRIBUTING.md).
exec "$(dirname "$0")/scripts/dev/up.sh" "$@"
