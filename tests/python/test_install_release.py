"""Install-from-release: packaging/release/package.sh + the two installers.

Builds a fake release directory (fake `hub-*` shell scripts, the real Python
remote bundle, SHA256SUMS) with the same script the release workflow uses,
then installs from it through file:// URLs. No network, no Rust build, no NATS.
"""

from __future__ import annotations

import os
import platform
import shutil
import stat
import subprocess
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
PACKAGE = REPO / "packaging" / "release" / "package.sh"
INSTALL_REMOTE = REPO / "scripts" / "install_remote.sh"
INSTALL_BUNDLE = REPO / "packaging" / "remote" / "install.sh"
VERSION = "v9.9.9-test"

TARGETS = {
    ("Linux", "x86_64"): "x86_64-unknown-linux-gnu",
    ("Linux", "aarch64"): "aarch64-unknown-linux-gnu",
    ("Darwin", "arm64"): "aarch64-apple-darwin",
}
HOST_TARGET = TARGETS.get((platform.system(), platform.machine()))

pytestmark = pytest.mark.skipif(shutil.which("bash") is None, reason="needs bash")


def _exe(path: Path, body: str) -> None:
    path.write_text(body)
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def _run(cmd: list[str], env: dict[str, str] | None = None, cwd: Path | None = None):
    return subprocess.run(
        cmd, env=env, cwd=cwd, capture_output=True, text=True, timeout=180, check=False
    )


@pytest.fixture
def fake_release(tmp_path: Path) -> Path:
    """dist/ with one binary tarball for this host, the remote bundle, SHA256SUMS."""
    if HOST_TARGET is None:
        pytest.skip(f"no release target for {platform.system()}/{platform.machine()}")
    bins = tmp_path / "bins"
    bins.mkdir()
    for name in ("hub-delegate", "hub-watch", "hub-server"):
        _exe(bins / name, f'#!/bin/sh\necho "{name} $*"\n')
    (bins / "hub-server.d").write_text("cargo dep-info, must not be packaged\n")
    dist = tmp_path / "dist"
    for args in (
        ["binaries", VERSION, HOST_TARGET, str(bins), str(dist)],
        ["remote-bundle", VERSION, str(dist)],
        ["checksums", str(dist)],
    ):
        r = _run(["bash", str(PACKAGE), *args])
        assert r.returncode == 0, r.stderr
    return dist


@pytest.fixture
def home_env(tmp_path: Path) -> dict[str, str]:
    """Isolated HOME plus a fake `cargo` that records it was called."""
    home = tmp_path / "home"
    home.mkdir()
    fakebin = tmp_path / "fakebin"
    fakebin.mkdir()
    _exe(
        fakebin / "cargo",
        "#!/bin/sh\n"
        'echo called > "$HOME/cargo-called"\n'
        'mkdir -p "$CARGO_TARGET_DIR/release"\n'
        "printf '#!/bin/sh\\necho from-source\\n' > \"$CARGO_TARGET_DIR/release/hub-delegate\"\n"
        'chmod +x "$CARGO_TARGET_DIR/release/hub-delegate"\n',
    )
    env = {k: v for k, v in os.environ.items() if not k.startswith(("NATS_HUB_", "CARGO_TARGET"))}
    env.pop("HUB_DELEGATE_BIN", None)
    env["HOME"] = str(home)
    env["PATH"] = f"{fakebin}{os.pathsep}{env.get('PATH', '')}"
    return env


def _installed(env: dict[str, str], name: str = "hub-delegate") -> Path:
    return Path(env["HOME"]) / ".local" / "share" / "nats-hub" / "bin" / name


def _wrapper(env: dict[str, str]) -> Path:
    return Path(env["HOME"]) / "bin" / "hub-delegate-remote"


def test_package_layout(fake_release: Path) -> None:
    sums = (fake_release / "SHA256SUMS").read_text().splitlines()
    names = sorted(line.split()[1] for line in sums)
    assert names == sorted(
        [f"nats-hub-{VERSION}-{HOST_TARGET}.tar.gz", f"nats-hub-remote-{VERSION}.tar.gz"]
    )
    listing = _run(["tar", "-tzf", str(fake_release / f"nats-hub-{VERSION}-{HOST_TARGET}.tar.gz")])
    entries = {e.rstrip("/") for e in listing.stdout.split()}
    top = f"nats-hub-{VERSION}-{HOST_TARGET}"
    assert {f"{top}/hub-delegate", f"{top}/hub-watch", f"{top}/README.md"} <= entries
    assert f"{top}/hub-server.d" not in entries
    bundle = _run(["tar", "-tzf", str(fake_release / f"nats-hub-remote-{VERSION}.tar.gz")])
    assert f"nats-hub-remote-{VERSION}/remote_agent_adapter.py" in bundle.stdout
    assert f"nats-hub-remote-{VERSION}/install.sh" in bundle.stdout


def test_install_remote_from_release(fake_release: Path, home_env: dict[str, str]) -> None:
    r = _run(
        ["bash", str(INSTALL_REMOTE), "10.0.0.5",
         "--release-base-url", f"file://{fake_release}", "--release-only"],
        env=home_env,
    )
    assert r.returncode == 0, r.stdout + r.stderr
    assert "sha256 verified" in r.stdout
    assert "plaintext" in r.stdout  # non-loopback nats:// is called out
    assert _installed(home_env).exists() and _installed(home_env, "hub-watch").exists()
    assert not (Path(home_env["HOME"]) / "cargo-called").exists()
    out = _run([str(_wrapper(home_env)), "--to", "w1"], env=home_env).stdout
    assert out.strip() == "hub-delegate --nats-url nats://10.0.0.5:4222 --to w1"


def test_full_url_is_baked_verbatim(fake_release: Path, home_env: dict[str, str]) -> None:
    r = _run(
        ["bash", str(INSTALL_REMOTE), "wss://hub.example.com:8443",
         "--release-base-url", f"file://{fake_release}"],
        env=home_env,
    )
    assert r.returncode == 0, r.stdout + r.stderr
    assert "plaintext" not in r.stdout
    out = _run([str(_wrapper(home_env))], env=home_env).stdout
    assert out.strip() == "hub-delegate --nats-url wss://hub.example.com:8443"


def test_checksum_mismatch_is_fatal(fake_release: Path, home_env: dict[str, str]) -> None:
    tarball = fake_release / f"nats-hub-{VERSION}-{HOST_TARGET}.tar.gz"
    with tarball.open("ab") as fh:
        fh.write(b"tampered")
    r = _run(
        ["bash", str(INSTALL_REMOTE), "hub", "--release-base-url", f"file://{fake_release}"],
        env=home_env,
    )
    assert r.returncode != 0
    assert "checksum mismatch" in r.stderr
    assert not _installed(home_env).exists()
    # A mismatch must never silently fall back to a source build.
    assert not (Path(home_env["HOME"]) / "cargo-called").exists()


def test_missing_release_falls_back_to_source(tmp_path: Path, home_env: dict[str, str]) -> None:
    empty = tmp_path / "empty"
    empty.mkdir()
    r = _run(
        ["bash", str(INSTALL_REMOTE), "hub", "--release-base-url", f"file://{empty}"],
        env=home_env,
    )
    assert r.returncode == 0, r.stdout + r.stderr
    assert "Falling back to building from source" in r.stdout
    assert (Path(home_env["HOME"]) / "cargo-called").exists()
    assert _run([str(_installed(home_env))]).stdout.strip() == "from-source"


def test_release_only_does_not_fall_back(tmp_path: Path, home_env: dict[str, str]) -> None:
    empty = tmp_path / "empty"
    empty.mkdir()
    r = _run(
        ["bash", str(INSTALL_REMOTE), "hub", "--release-only",
         "--release-base-url", f"file://{empty}"],
        env=home_env,
    )
    assert r.returncode != 0
    assert not (Path(home_env["HOME"]) / "cargo-called").exists()


def test_plain_http_download_refused(home_env: dict[str, str]) -> None:
    r = _run(
        ["bash", str(INSTALL_REMOTE), "hub", "--release-base-url", "http://example.invalid/r"],
        env=home_env,
    )
    assert r.returncode != 0
    assert "refusing non-TLS" in r.stderr


def _standalone_bundle_installer(tmp_path: Path) -> Path:
    """install.sh copied away from any checkout, as if fetched on its own."""
    d = tmp_path / "standalone"
    d.mkdir()
    shutil.copy(INSTALL_BUNDLE, d / "install.sh")
    return d / "install.sh"


def test_remote_bundle_from_release(
    fake_release: Path, home_env: dict[str, str], tmp_path: Path
) -> None:
    target = tmp_path / "remote"
    r = _run(
        ["bash", str(_standalone_bundle_installer(tmp_path)), str(target),
         "--from-release", VERSION, "--release-base-url", f"file://{fake_release}", "--no-pip"],
        env=home_env,
    )
    assert r.returncode == 0, r.stdout + r.stderr
    assert f"release {VERSION} (sha256 verified)" in r.stdout
    for rel in ("remote_agent_adapter.py", "worker_backends/presets.py", "requirements-remote.txt"):
        assert (target / rel).read_bytes() == (REPO / rel).read_bytes()
    assert (target / ".venv" / "bin" / "python").exists()
    # The installed file set must be a complete import closure on its own:
    # run the adapter from the target dir with no repo on sys.path (-E -s).
    # (Regression: FILES.txt lacked worker_backends/proc.py.)
    probe = _run(
        [sys.executable, "-E", "-s", "-c",
         "import remote_agent_adapter, worker_backends.presets; print('ok')"],
        cwd=target,
    )
    assert probe.returncode == 0, probe.stderr
    assert str(REPO) not in probe.stderr


def test_remote_bundle_mismatch_is_fatal(
    fake_release: Path, home_env: dict[str, str], tmp_path: Path
) -> None:
    with (fake_release / f"nats-hub-remote-{VERSION}.tar.gz").open("ab") as fh:
        fh.write(b"tampered")
    target = tmp_path / "remote"
    r = _run(
        ["bash", str(_standalone_bundle_installer(tmp_path)), str(target),
         "--from-release", VERSION, "--release-base-url", f"file://{fake_release}", "--no-pip"],
        env=home_env,
    )
    assert r.returncode != 0
    assert "checksum mismatch" in r.stderr
    assert not (target / "remote_agent_adapter.py").exists()
