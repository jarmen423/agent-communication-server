#!/usr/bin/env bash
# package.sh — build the nats-hub release assets from already-built binaries.
#
# Used by .github/workflows/release.yml and runnable locally. It never builds
# Rust and never publishes anything; it only lays out tarballs and checksums.
#
#   package.sh binaries <version> <target> <bin-dir> <out-dir>
#       Tar every `hub-*` executable in <bin-dir> (a cargo `release/` dir)
#       plus README.md and any LICENSE* into
#       <out-dir>/nats-hub-<version>-<target>.tar.gz
#
#   package.sh remote-bundle <version> <out-dir>
#       Tar the Python remote-worker bundle (packaging/remote/FILES.txt +
#       install.sh) into <out-dir>/nats-hub-remote-<version>.tar.gz
#
#   package.sh checksums <dir>
#       Write <dir>/SHA256SUMS over every *.tar.gz in <dir>, then verify it.
#
# Asset naming is a contract with scripts/install_remote.sh and
# packaging/remote/install.sh: they read SHA256SUMS and pick the line ending
# in `-<target>.tar.gz` (binaries) or starting `nats-hub-remote-` (bundle).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

die() { echo "package.sh: $*" >&2; exit 1; }

STAGES=()
STAGE=""
cleanup() { local d; for d in "${STAGES[@]+"${STAGES[@]}"}"; do rm -rf "$d"; done; }
trap cleanup EXIT
# new_stage: make a temp dir, register it for cleanup, return it in $STAGE.
# (Not via $(...): a subshell would lose the STAGES registration.)
new_stage() { STAGE="$(mktemp -d)"; STAGES+=("$STAGE"); }

sha256_of() {  # sha256_of <file> → hex digest
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    die "need sha256sum or shasum"
  fi
}

# Deterministic-ish tarball: sorted entries, fixed owner. (GNU tar and bsdtar
# differ in flags, so only use the portable subset.)
make_tarball() {  # make_tarball <staging-parent> <dir-name> <out-file>
  local parent="$1" name="$2" out="$3"
  (cd "$parent" && tar -czf "$out" "$name")
}

copy_license_files() {  # copy_license_files <dest-dir>
  local f
  for f in "$REPO_ROOT"/LICENSE*; do
    [[ -f "$f" ]] && cp "$f" "$1/"
  done
  return 0
}

cmd_binaries() {
  [[ $# -eq 4 ]] || die "usage: binaries <version> <target> <bin-dir> <out-dir>"
  local version="$1" target="$2" bin_dir="$3" out_dir="$4"
  [[ -d "$bin_dir" ]] || die "bin dir not found: $bin_dir"
  mkdir -p "$out_dir"
  out_dir="$(cd "$out_dir" && pwd)"

  local name="nats-hub-${version}-${target}"
  local stage
  new_stage
  stage="$STAGE"
  mkdir -p "$stage/$name"

  local count=0 f base
  for f in "$bin_dir"/hub-*; do
    base="$(basename "$f")"
    # Skip cargo's side files (hub-server.d, hub-server.pdb, …).
    [[ -f "$f" && -x "$f" && "$base" != *.* ]] || continue
    cp "$f" "$stage/$name/$base"
    count=$((count + 1))
  done
  (( count > 0 )) || die "no hub-* executables in $bin_dir"
  [[ -x "$stage/$name/hub-delegate" ]] || die "hub-delegate missing from $bin_dir"

  cp "$REPO_ROOT/README.md" "$stage/$name/"
  copy_license_files "$stage/$name"
  make_tarball "$stage" "$name" "$out_dir/$name.tar.gz"
  echo "wrote $out_dir/$name.tar.gz ($count binaries)"
}

# FILES.txt entries: non-empty lines, comments stripped.
remote_files() {
  sed -e 's/#.*//' -e 's/[[:space:]]*$//' "$REPO_ROOT/packaging/remote/FILES.txt" | grep -v '^$'
}

cmd_remote_bundle() {
  [[ $# -eq 2 ]] || die "usage: remote-bundle <version> <out-dir>"
  local version="$1" out_dir="$2"
  mkdir -p "$out_dir"
  out_dir="$(cd "$out_dir" && pwd)"

  local name="nats-hub-remote-${version}"
  local stage
  new_stage
  stage="$STAGE"
  mkdir -p "$stage/$name"

  local rel
  while IFS= read -r rel; do
    [[ -f "$REPO_ROOT/$rel" ]] || die "FILES.txt lists a missing file: $rel"
    mkdir -p "$stage/$name/$(dirname "$rel")"
    cp "$REPO_ROOT/$rel" "$stage/$name/$rel"
  done < <(remote_files)

  cp "$REPO_ROOT/packaging/remote/install.sh" "$stage/$name/install.sh"
  cp "$REPO_ROOT/packaging/remote/FILES.txt" "$stage/$name/FILES.txt"
  cp "$REPO_ROOT/packaging/remote/README.md" "$stage/$name/README.md"
  chmod +x "$stage/$name/install.sh"
  copy_license_files "$stage/$name"
  make_tarball "$stage" "$name" "$out_dir/$name.tar.gz"
  echo "wrote $out_dir/$name.tar.gz"
}

cmd_checksums() {
  [[ $# -eq 1 ]] || die "usage: checksums <dir>"
  local dir="$1"
  [[ -d "$dir" ]] || die "not a directory: $dir"
  local files=()
  local f
  for f in "$dir"/*.tar.gz; do
    [[ -f "$f" ]] && files+=("$(basename "$f")")
  done
  (( ${#files[@]} > 0 )) || die "no *.tar.gz in $dir"

  : >"$dir/SHA256SUMS"
  for f in "${files[@]}"; do
    printf '%s  %s\n' "$(sha256_of "$dir/$f")" "$f" >>"$dir/SHA256SUMS"
  done

  # Verify what we just wrote with the same tool installers use.
  local sum name
  while read -r sum name; do
    [[ "$(sha256_of "$dir/$name")" == "$sum" ]] || die "checksum mismatch after write: $name"
  done <"$dir/SHA256SUMS"
  echo "wrote $dir/SHA256SUMS (${#files[@]} assets, verified)"
}

sub="${1:-}"
[[ -n "$sub" ]] || die "usage: package.sh {binaries|remote-bundle|checksums} ..."
shift
case "$sub" in
  binaries) cmd_binaries "$@" ;;
  remote-bundle) cmd_remote_bundle "$@" ;;
  checksums) cmd_checksums "$@" ;;
  -h|--help) awk 'NR>1 && /^#/ {sub(/^# ?/, ""); print; next} NR>1 {exit}' "${BASH_SOURCE[0]}" ;;
  *) die "unknown subcommand: $sub" ;;
esac
