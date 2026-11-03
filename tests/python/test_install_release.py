"""Install-from-release: packaging/release/package.sh + the two installers.

Builds a fake release directory (fake `acs`/`hub-*` shell scripts, the real
Python remote bundle, SHA256SUMS) with the same script the release workflow
uses, then installs from it through file:// URLs. No network, no Rust build,
no NATS. Every run gets its own HOME.
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
LICENSE_FILES = (
    "LICENSE", "NOTICE", "LICENSING.md", "LICENSES/Apache-2.0.txt", "LICENSES/BUSL-1.1.txt",
)

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
    for name in ("acs", "hub-delegate", "hub-watch", "hub-server"):
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
        'for b in hub-delegate acs; do\n'
        '  case " $* " in *" $b "*) ;; *) continue ;; esac\n'
        "  printf '#!/bin/sh\\necho from-source\\n' > \"$CARGO_TARGET_DIR/release/$b\"\n"
        '  chmod +x "$CARGO_TARGET_DIR/release/$b"\n'
        'done\n',
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


def _link(env: dict[str, str], name: str) -> Path:
    return Path(env["HOME"]) / ".local" / "bin" / name


def _install(fake_release: Path, env: dict[str, str], *args: str):
    return _run(
        ["bash", str(INSTALL_REMOTE), *args,
         "--release-base-url", f"file://{fake_release}", "--release-only"],
        env=env,
    )


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
    bundle_entries = {e.rstrip("/") for e in bundle.stdout.split()}
    for root, listing in ((top, entries), (f"nats-hub-remote-{VERSION}", bundle_entries)):
        for rel in LICENSE_FILES:
            assert f"{root}/{rel}" in listing, f"{rel} missing from {root}"


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
    # Self-host still links everything, the wrapper too, and names hosted once.
    assert os.readlink(_link(home_env, "hub-delegate-remote")) == str(_wrapper(home_env))
    assert os.readlink(_link(home_env, "acs")) == str(_installed(home_env, "acs"))
    assert "Next, on your hub (nats://10.0.0.5:4222)" in r.stdout
    assert r.stdout.count("acs login") == 1


def test_hosted_install_links_everything_and_leads_with_acs_login(
    fake_release: Path, home_env: dict[str, str]
) -> None:
    """B2 + B4: no HUB argument = hosted ACS; no wrapper, links, next steps."""
    r = _install(fake_release, home_env)
    assert r.returncode == 0, r.stdout + r.stderr
    for name in ("acs", "hub-delegate", "hub-watch", "hub-server"):
        link = _link(home_env, name)
        assert link.is_symlink() and os.readlink(link) == str(_installed(home_env, name))
        assert _run([str(link), "x"]).stdout.strip() == f"{name} x"
    assert not _wrapper(home_env).exists()
    assert not _link(home_env, "hub-delegate-remote").exists()
    nxt = r.stdout.split("Next, sign in to hosted ACS (https://acs.agentmemorylabs.com):", 1)[1]
    steps = [line.split("#")[0].strip() for line in nxt.splitlines()[1:5]]
    assert steps == ["acs login", "acs agent add <id>", "acs up", "acs exec <id> -- hub-agents"]
    self_host = [line for line in r.stdout.splitlines() if "elf-host" in line]
    assert len(self_host) == 2  # the banner and one closing line
    assert self_host[-1].startswith("Self-hosting a hub? Re-run with its URL: curl -fsSL ")


def test_piped_into_posix_sh(fake_release: Path, home_env: dict[str, str]) -> None:
    """`curl … | sh` runs the script with dash on Debian/Ubuntu."""
    sh = shutil.which("sh")
    if sh is None:
        pytest.skip("no sh")
    r = subprocess.run(
        [sh, "-s", "--", "--release-base-url", f"file://{fake_release}", "--release-only"],
        input=INSTALL_REMOTE.read_text(), env=home_env, capture_output=True, text=True,
        timeout=180, check=False,
    )
    assert r.returncode == 0, r.stdout + r.stderr
    assert "sha256 verified" in r.stdout and "acs login" in r.stdout
    assert os.readlink(_link(home_env, "acs")) == str(_installed(home_env, "acs"))


def test_rerun_is_idempotent(fake_release: Path, home_env: dict[str, str]) -> None:
    for args in ((), (), ("hub.lan",), ("hub.lan",), ()):
        r = _install(fake_release, home_env, *args)
        assert r.returncode == 0, r.stdout + r.stderr
        assert "left alone" not in r.stderr
        assert "linked:   " in r.stdout
    links = sorted(p.name for p in _link(home_env, "x").parent.iterdir())
    assert links == ["acs", "hub-delegate", "hub-delegate-remote", "hub-server", "hub-watch"]
    for name in links:
        assert _link(home_env, name).resolve().exists(), name


def test_foreign_files_in_local_bin_are_left_alone(
    fake_release: Path, home_env: dict[str, str], tmp_path: Path
) -> None:
    bin_dir = _link(home_env, "x").parent
    bin_dir.mkdir(parents=True)
    _exe(bin_dir / "acs", "#!/bin/sh\necho mine\n")
    other = tmp_path / "other-hub-watch"
    _exe(other, "#!/bin/sh\necho other\n")
    (bin_dir / "hub-watch").symlink_to(other)
    (bin_dir / "hub-delegate").symlink_to(tmp_path / "gone")  # dangling: replaced
    r = _install(fake_release, home_env)
    assert r.returncode == 0, r.stdout + r.stderr
    assert (bin_dir / "acs").read_text() == "#!/bin/sh\necho mine\n"
    assert os.readlink(bin_dir / "hub-watch") == str(other)
    assert os.readlink(bin_dir / "hub-delegate") == str(_installed(home_env))
    assert f"left alone: {bin_dir / 'acs'}" in r.stderr
    assert f"left alone: {bin_dir / 'hub-watch'}" in r.stderr


def test_path_line_only_when_local_bin_is_off_path(
    fake_release: Path, home_env: dict[str, str]
) -> None:
    home_env["SHELL"] = "/bin/zsh"
    r = _install(fake_release, home_env)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "is not on your PATH. Run this now and add it to ~/.zshrc:" in r.stdout
    assert '      export PATH="$HOME/.local/bin:$PATH"\n' in r.stdout
    on_path = {**home_env, "PATH": f"{_link(home_env, 'x').parent}{os.pathsep}{home_env['PATH']}"}
    r = _install(fake_release, on_path)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "not on your PATH" not in r.stdout
    assert "hides this" not in r.stdout


def test_shadowing_binary_earlier_on_path_is_named(
    fake_release: Path, home_env: dict[str, str], tmp_path: Path
) -> None:
    early = tmp_path / "early"
    early.mkdir()
    _exe(early / "acs", "#!/bin/sh\necho old\n")
    local_bin = _link(home_env, "x").parent
    env = {**home_env, "PATH": f"{early}{os.pathsep}{local_bin}{os.pathsep}{home_env['PATH']}"}
    r = _install(fake_release, env)
    assert r.returncode == 0, r.stdout + r.stderr
    assert f"{early / 'acs'} comes before {local_bin} on PATH and hides this acs" in r.stdout


def test_hosted_source_fallback_builds_acs(tmp_path: Path, home_env: dict[str, str]) -> None:
    empty = tmp_path / "empty"
    empty.mkdir()
    r = _run(["bash", str(INSTALL_REMOTE), "--release-base-url", f"file://{empty}"], env=home_env)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "-p acs --bin acs" in r.stdout
    for name in ("acs", "hub-delegate"):
        assert _run([str(_link(home_env, name))]).stdout.strip() == "from-source"


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
