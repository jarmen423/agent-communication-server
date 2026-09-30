#!/usr/bin/env bash
# Print the BINDGEN_EXTRA_CLANG_ARGS needed to build surrealdb-librocksdb-sys,
# or nothing if the toolchain is already fine.
#
# Why: bindgen uses libclang, which needs clang's builtin headers (stdbool.h,
# stddef.h). Machines with libclang but no full `clang` package (common on
# Ubuntu) fail with "'stdbool.h' file not found". GCC ships the same builtin
# headers, so we point bindgen at them.
set -euo pipefail

# Respect an explicit override.
if [[ -n "${BINDGEN_EXTRA_CLANG_ARGS:-}" ]]; then
  echo "$BINDGEN_EXTRA_CLANG_ARGS"
  exit 0
fi

# A full clang install carries its own resource headers — nothing to do.
if command -v clang >/dev/null 2>&1; then
  res="$(clang -print-resource-dir 2>/dev/null || true)"
  if [[ -n "$res" && -f "$res/include/stdbool.h" ]]; then
    exit 0
  fi
fi

# Fall back to GCC's builtin include dir.
if command -v gcc >/dev/null 2>&1; then
  inc="$(gcc -print-file-name=include 2>/dev/null || true)"
  if [[ -n "$inc" && -f "$inc/stdbool.h" ]]; then
    echo "-I$inc"
    exit 0
  fi
fi
