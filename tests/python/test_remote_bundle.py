"""packaging/remote/install.sh installs where `acs` looks and points hosted users at acs."""
import os
from pathlib import Path
import subprocess

REPO = Path(__file__).resolve().parents[2]


def test_bundle_defaults_to_the_acs_path_and_names_acs_worker_run(tmp_path):
    home = tmp_path / 'home'
    home.mkdir()
    env = {**os.environ, 'HOME': str(home)}
    p = subprocess.run(['bash', str(REPO / 'packaging/remote/install.sh'), '--no-pip'],
                       cwd=tmp_path, env=env, capture_output=True, text=True, timeout=120)
    assert p.returncode == 0, p.stdout + p.stderr
    target = home / '.local/share/nats-hub/remote'
    assert (target / 'remote_agent_adapter.py').is_file() and (target / '.venv/bin/python').exists()
    assert not (home / 'nats-hub-remote').exists()
    assert 'acs worker run claude --identity' in p.stdout
    assert f'cd "{target}"' in p.stdout
