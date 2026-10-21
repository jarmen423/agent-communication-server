"""The binary packager and release installer include the standalone connector."""
import hashlib
import os
from pathlib import Path
import subprocess
import tarfile

REPO = Path(__file__).resolve().parents[2]


def test_release_package_and_install_include_acs(tmp_path):
    bins, dist, home = (tmp_path / n for n in ('bin', 'dist', 'home'))
    bins.mkdir()
    for name in ('acs', 'hub-delegate'):
        binary = bins / name
        binary.write_text('#!/bin/sh\nexit 0\n')
        binary.chmod(0o755)
    arch = {'arm64': 'aarch64', 'aarch64': 'aarch64', 'x86_64': 'x86_64'}[os.uname().machine]
    target = arch + ('-apple-darwin' if os.uname().sysname == 'Darwin' else '-unknown-linux-gnu')
    subprocess.run(['bash', 'packaging/release/package.sh', 'binaries', 'v-test', target,
                    str(bins), str(dist)], cwd=REPO, check=True, capture_output=True)
    asset = next(dist.glob('*.tar.gz'))
    with tarfile.open(asset) as archive:
        names = archive.getnames()
        assert any(n.endswith('/acs') for n in names)
    (dist / 'SHA256SUMS').write_text(hashlib.sha256(asset.read_bytes()).hexdigest() + '  ' + asset.name + '\n')
    env = {**os.environ, 'HOME': str(home)}
    p = subprocess.run(['bash', 'scripts/install_remote.sh', '127.0.0.1', '--release-only',
                        '--release-base-url', dist.as_uri()], cwd=REPO, env=env,
                       capture_output=True, text=True)
    assert p.returncode == 0, p.stdout + p.stderr
    assert (home / '.local/share/nats-hub/bin/acs').is_file()
