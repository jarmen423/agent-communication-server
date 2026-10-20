# Releasing

Releases are built by [`.github/workflows/release.yml`](../.github/workflows/release.yml)
and laid out by [`packaging/release/package.sh`](../packaging/release/package.sh).
Nothing is built or published from a laptop.

## What a release contains

| Asset | Contents |
|---|---|
| `nats-hub-<tag>-x86_64-unknown-linux-gnu.tar.gz` | Every `hub-*` binary (`hub-server`, `hub-delegate`, `hub-tui`, …), `README.md`, any `LICENSE*` |
| `nats-hub-<tag>-aarch64-unknown-linux-gnu.tar.gz` | Same, linux arm64 |
| `nats-hub-<tag>-aarch64-apple-darwin.tar.gz` | Same, macOS Apple Silicon |
| `nats-hub-remote-<tag>.tar.gz` | Python remote-worker bundle: the files in `packaging/remote/FILES.txt` plus `install.sh` |
| `SHA256SUMS` | `sha256  filename` for every tarball above |

Each tarball unpacks to one directory named like the tarball. The Linux
binaries are built on Ubuntu 22.04 and need glibc 2.35 or newer (Ubuntu 22.04+,
Debian 12+). The macOS binary is built on the `macos-latest` arm64 runner. No
Intel-mac build is published; the installer falls back to a source build there.

Binaries are built with `cargo build --release --locked --bins --features tui`
and stripped (`CARGO_PROFILE_RELEASE_STRIP=symbols`). Each one is smoke-run
with `--help` on its own platform before it's packaged.

## Cut a release

1. Bump `version` in `Cargo.toml`. The workflow refuses a tag that doesn't match,
   although `v0.2.0-rc.1` is accepted for version `0.2.0`. Keep the plugin
   manifests' `version` in step if they changed.
2. Merge to `main` and make sure CI is green.
3. Tag and push:

   ```bash
   git tag -a v0.2.0 -m "nats-hub v0.2.0"
   git push origin v0.2.0
   ```

4. The workflow runs `plan` → `build` (three native runners in parallel) →
   `assemble` (remote bundle, `SHA256SUMS`, installer smoke tests) → `publish`.
   `publish` creates the GitHub Release with generated notes. A tag with a `-`
   (`v0.2.0-rc.1`) is marked as a pre-release.

If a tag run fails after the tag is pushed, fix the problem on `main`, then
either move the tag or re-run the release. To re-run, dispatch the workflow
**on the tag ref** with `dry_run=false`. Assets are then uploaded with
`--clobber`.

## Dry runs (no tag, nothing published)

- **On every push** to a branch that touches `release.yml`, `packaging/**`,
  `scripts/install_remote.sh`, `Cargo.toml` or `Cargo.lock`, the full pipeline
  runs except `publish`.
- **Manually:**

  ```bash
  gh workflow run release.yml --ref <branch> -f dry_run=true            # version label: v<Cargo version>-dryrun
  gh workflow run release.yml --ref <branch> -f version=v0.2.0-test      # custom label
  gh run watch
  ```

  `dry_run=false` is rejected unless the ref is a `v*` tag.

The packaged assets from a dry run land in the run's `release-assets`
artifact, and the run summary lists their SHA-256 sums.

## Install from a release

```bash
# client CLIs (hub-delegate & co.) + a hub-delegate-remote wrapper
bash scripts/install_remote.sh wss://hub.example.com:8080                 # latest release
bash scripts/install_remote.sh wss://hub.example.com:8080 --from-release v0.2.0
bash scripts/install_remote.sh hub.lan --from-source                      # skip releases

# Python remote-worker bundle
bash packaging/remote/install.sh ~/nats-hub-remote --from-release latest
```

Both installers download `SHA256SUMS` and the asset over **HTTPS only**. Curl
runs with `--proto =https`, so redirects are TLS too. They verify the SHA-256
before extracting anything.

- **Checksum mismatch:** the install aborts. It never falls back to a source
  build.
- **Release unavailable** (no release yet, no asset for your platform, network
  error): `install_remote.sh` builds `hub-delegate` from source, unless you
  pass `--release-only`. `packaging/remote/install.sh` uses the local checkout
  if there is one.
- **Mirrors and air-gapped installs:** `--release-base-url https://mirror/…`
  or `file:///path/to/dir`. The directory must hold `SHA256SUMS` and the
  tarballs. Plain `http://` is refused.

To check a download by hand:

```bash
sha256sum -c --ignore-missing SHA256SUMS      # macOS: shasum -a 256 -c --ignore-missing SHA256SUMS
```

## Test the packaging locally

`tests/python/test_install_release.py` builds a fake release (shell-script
"binaries", the real remote bundle, `SHA256SUMS`) with `package.sh`. It then
installs from it through `file://` URLs. It covers the happy path, full-URL
baking, a checksum mismatch (fatal, no fallback), a missing release (falls back
to source), `--release-only` and refused `http://`. It needs no Rust build and
no NATS:

```bash
.venv/bin/python -m pytest -q tests/python/test_install_release.py
```
