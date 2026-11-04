"""Device upgrades preserve identities and verify service recovery."""
import base64
import fcntl
import json
import os
from pathlib import Path
import subprocess
import time

import pytest

from test_acs_cli import ACS, REPO, acs, device_stack, headless_login
from test_acs_review import legacy


def configure(root, identity='bot'):
    value = json.loads((root / 'state.json').read_text())
    value['agents'] = {identity: dict(role='worker', public_key='U', expires_at=9999999999)}
    (root / 'state.json').write_text(json.dumps(value))
    (root / 'creds').mkdir(exist_ok=True)
    (root / f'creds/{identity}.creds').write_text('original credential')
    return value


def test_unconfigured_files_and_existing_reserved_identity(tmp_path):
    root = tmp_path / 'acs'
    env = legacy(root)
    configure(root, 'manifest')
    for name in ['backup copy', 'stray']:
        (root / f'creds/{name}.creds').write_text('leave me alone')
    out = acs(env, 'status')
    assert 'reserved identity manifest' in out.stderr
    for name in ['backup copy', 'stray']:
        path = root / f'creds/{name}.creds'
        assert str(path) in out.stderr
        assert path.read_text() == 'leave me alone' and not path.is_symlink()
    assert 'manifest' in json.loads((root / 'tenants/A/state.json').read_text())['agents']
    assert (root / 'creds/manifest.creds').is_symlink()


@pytest.mark.parametrize('agents', [{}, {'bot': dict(public_key='different')}])
def test_restored_state_must_preserve_all_public_keys(tmp_path, agents):
    root = tmp_path / 'acs'
    env = legacy(root)
    configure(root)
    target = root / 'tenants/A'
    (target / 'creds').mkdir(parents=True)
    (target / 'creds/bot.creds').write_text('original credential')
    (target / 'state.json').write_text(json.dumps(dict(nats_url='nats://unused', agents=agents)))
    before = {p: p.read_bytes() for p in root.rglob('*') if p.is_file()}
    error = acs(env, 'status', ok=False).stderr
    assert str(target / 'state.json') in error and 'bot' in error and 'restore' in error
    assert all(p.read_bytes() == content for p, content in before.items())
    assert not (root / 'device.json').exists()


def credential(exp, iat):
    # A valid user seed, fixed only within this temporary test directory.
    from nkeys import encode_seed, PREFIX_BYTE_USER
    seed = encode_seed(os.urandom(32), PREFIX_BYTE_USER).decode()
    def encode(expiry, issued):
        claims = base64.urlsafe_b64encode(json.dumps(dict(exp=expiry, iat=issued)).encode()).decode().rstrip('=')
        return f'-----BEGIN NATS USER JWT-----\nheader.{claims}.sig\n------END NATS USER JWT------\n\n-----BEGIN USER NKEY SEED-----\n{seed}\n------END USER NKEY SEED------\n'
    return encode(exp, iat), encode


@pytest.mark.parametrize('source_age,target_age', [((100, 1), (200, 1)), ((200, 1), (100, 1)), ((200, 2), (200, 1))])
def test_same_seed_keeps_newer_jwt(tmp_path, source_age, target_age):
    root = tmp_path / 'acs'
    env = legacy(root)
    value = configure(root)
    source, encode = credential(*source_age)
    other = encode(*target_age)
    target = root / 'tenants/A'
    (target / 'creds').mkdir(parents=True)
    (target / 'state.json').write_text(json.dumps(value))
    (target / 'creds/bot.creds').write_text(other)
    (root / 'creds/bot.creds').write_text(source)
    acs(env, 'status')
    expected = source if source_age > target_age else other
    assert (target / 'creds/bot.creds').read_text() == expected
    assert (root / 'creds/bot.creds').read_text() == expected


@pytest.mark.parametrize('expired', [False, True])
def test_logout_revokes_legacy_device_despite_restore_conflict(device_stack, expired):
    env, _, _ = device_stack
    headless_login(env)
    acs(env, 'agent', 'add', 'bot')
    root = Path(env['ACS_HOME'])
    value = json.loads((root / 'device.json').read_text())
    target = root / 'tenants' / value['tenant']
    value['agents'] = json.loads((target / 'state.json').read_text())['agents']
    if expired:
        value['access_expires_at'] = 0
    (root / 'state.json').write_text(json.dumps(value))
    (root / 'device.json').unlink()
    (root / 'creds').mkdir()
    (root / 'creds/bot.creds').write_text('conflicting seed')
    error = acs(env, 'status', ok=False).stderr
    assert 'bot.creds' in error and 'restore' in error
    assert 'Logged out' in acs(env, 'logout').stdout
    assert not (root / 'tenants').exists() and not (root / 'state.json').exists()
    assert not (root / 'refresh.token').exists()


def test_conflicting_tenant_does_not_stop_daemon_or_status(tmp_path):
    root = tmp_path / 'acs'
    env = legacy(root)
    acs(env, 'status')
    device = json.loads((root / 'device.json').read_text())
    device['tenants'].append(dict(tenant='B'))
    (root / 'device.json').write_text(json.dumps(device))
    (root / 'tenants/a').mkdir()
    (root / 'tenants/B').mkdir()
    (root / 'tenants/B/state.json').write_text(json.dumps(dict(nats_url='nats://unused', agents={
        'bot': dict(role='worker', public_key='U', expires_at=0)})))
    out = acs(env, 'status')
    assert 'Tenant: B' in out.stdout and 'Tenant A state unavailable' in out.stderr
    proc = subprocess.Popen([str(ACS), 'up', '--foreground'], env=env, cwd=REPO,
                            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    try:
        time.sleep(3)
        assert proc.poll() is None
    finally:
        proc.terminate()
        _, err = proc.communicate(timeout=5)
    assert 'Tenant A state unavailable' in err and 'Refresh bot failed' in err


@pytest.mark.skipif(not os.sys.platform.startswith('linux'), reason='systemd user service')
@pytest.mark.parametrize('mode', ['start', 'no-start', 'foreign'])
def test_upgrade_reinstalls_only_own_service_and_checks_daemon(tmp_path, mode):
    root = tmp_path / 'acs'
    env = legacy(root)
    configure(root)
    home = tmp_path / 'home'
    unit = home / '.config/systemd/user/acs.service'
    unit.parent.mkdir(parents=True)
    old = f'[Service]\nExecStart=/old/acs up --foreground\nEnvironment="ACS_HOME={root if mode != "foreign" else tmp_path / "other"}"\n'
    unit.write_text(old)
    bindir = tmp_path / 'bin'
    bindir.mkdir()
    manager = bindir / 'systemctl'
    manager.write_text(f'''#!{os.sys.executable}
import os, pathlib, shlex, subprocess, time
root = pathlib.Path(os.environ['ACS_HOME'])
assert (root / 'device.json').exists()
with (root / 'calls').open('a') as f: f.write(' '.join(os.sys.argv[1:]) + '\\n')
print('SERVICE OUTPUT MUST NOT ENTER EVAL')
if os.sys.argv[2] == 'restart' and {mode == 'start'!r}:
    unit = pathlib.Path(os.environ['HOME']) / '.config/systemd/user/acs.service'
    lines = dict(line.split('=', 1) for line in unit.read_text().splitlines() if '=' in line)
    args = shlex.split(lines['ExecStart'])
    key, value = shlex.split(lines['Environment'])[0].split('=', 1)
    # Let the simulated old process release its daemon lock.
    time.sleep(.3)
    subprocess.Popen(args, env={{**os.environ, key: value}}, stdin=subprocess.DEVNULL,
                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
''')
    manager.chmod(0o755)
    env.update(HOME=str(home), PATH=str(bindir) + ':' + env['PATH'])
    with (root / 'daemon.lock').open('w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        proc = subprocess.Popen([str(ACS), 'env', 'bot'], env=env, cwd=REPO,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        for _ in range(200):
            if (root / 'device.json').exists() or proc.poll() is not None:
                break
            time.sleep(.01)
        fcntl.flock(lock, fcntl.LOCK_UN)
        out, err = proc.communicate(timeout=20)
    try:
        assert proc.returncode == 0
        assert all(line.startswith(('export ', 'unset ')) for line in out.splitlines())
        if mode == 'start':
            assert 'WARNING' not in err
            assert (root / 'daemon.pid').exists()
            assert str(ACS) in unit.read_text() and '/old/acs' not in unit.read_text()
        else:
            assert 'WARNING' in err and 'run acs up' in err
        if mode == 'foreign':
            assert unit.read_text() == old and not (root / 'calls').exists()
        else:
            assert '--user daemon-reload' in (root / 'calls').read_text()
    finally:
        if (root / 'daemon.pid').exists():
            acs(env, 'down')


def test_different_seeds_abort_with_named_recovery_before_mutation(tmp_path):
    root = tmp_path / 'acs'
    env = legacy(root)
    value = configure(root)
    source, _ = credential(100, 1)
    other, _ = credential(200, 2)
    target = root / 'tenants/A'
    (target / 'creds').mkdir(parents=True)
    (target / 'state.json').write_text(json.dumps(value))
    (target / 'creds/bot.creds').write_text(other)
    (root / 'creds/bot.creds').write_text(source)
    error = acs(env, 'status', ok=False).stderr
    assert str(target / 'creds/bot.creds') in error
    assert str(root / 'creds/bot.creds') in error and 'restore' in error
    assert (root / 'creds/bot.creds').read_text() == source
    assert (target / 'creds/bot.creds').read_text() == other
    assert not (root / 'state.json.bak').exists()
    assert not (root / 'device.json').exists()


@pytest.mark.skipif(not os.sys.platform.startswith('linux'), reason='Linux fsync fault injection')
@pytest.mark.parametrize('directory', ['tenants/A/creds', 'tenants/A', 'tenants'])
def test_destination_sync_failure_precedes_credential_link_swap(tmp_path, directory):
    import shutil
    compiler = shutil.which('cc')
    if not compiler:
        pytest.skip('C compiler required for fsync fault injection')
    root = tmp_path / 'acs'
    env = legacy(root)
    configure(root)
    source = tmp_path / 'sync_fault.c'
    library = tmp_path / 'sync_fault.so'
    source.write_text(r'''
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
int fsync(int fd) {
    char proc[64], path[4096];
    snprintf(proc, sizeof(proc), "/proc/self/fd/%d", fd);
    ssize_t n = readlink(proc, path, sizeof(path) - 1);
    if (n >= 0) {
        path[n] = 0;
        const char *target = getenv("SYNC_FAULT_PATH");
        if (target && strcmp(target, path) == 0) {
            errno = EIO;
            return -1;
        }
    }
    int (*real_sync)(int) = dlsym(RTLD_NEXT, "fsync");
    return real_sync(fd);
}
''')
    subprocess.run([compiler, '-shared', '-fPIC', '-o', str(library), str(source), '-ldl'], check=True)
    env.update(LD_PRELOAD=str(library), SYNC_FAULT_PATH=str(root / directory))
    error = acs(env, 'status', ok=False).stderr
    assert 'migration' in error
    assert not (root / 'creds/bot.creds').is_symlink()
    assert (root / 'creds/bot.creds').read_text() == 'original credential'
    assert not (root / 'device.json').exists()
    # A later run can complete safely after the filesystem recovers.
    env.pop('LD_PRELOAD')
    acs(env, 'status')
    assert (root / 'creds/bot.creds').is_symlink()
