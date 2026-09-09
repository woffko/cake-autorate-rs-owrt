#!/usr/bin/env python3
"""Real-UCI regression; all config/delta files live in a temporary directory.

Set CAKE_TEST_DAEMON and CAKE_TEST_UCI to the inspected executables. An optional
CAKE_TEST_MUSL_LOADER / CAKE_TEST_LIB_DIR runs SDK UCI on a glibc development host.
No installed system config is read or written by this test.
"""
import fcntl
import os
import pathlib
import shlex
import subprocess
import tempfile


def main():
    daemon = os.environ['CAKE_TEST_DAEMON']
    uci_binary = os.environ['CAKE_TEST_UCI']
    loader = os.environ.get('CAKE_TEST_MUSL_LOADER')
    uci_prefix = ([loader, '--library-path', os.environ['CAKE_TEST_LIB_DIR']] if loader else []) + [uci_binary]
    with tempfile.TemporaryDirectory(prefix='cake-status-uci-') as directory:
        root = pathlib.Path(directory)
        config, override, delta, session, clean = [root / name for name in ('config', 'override', 'delta', 'session', 'clean')]
        for path in (config, override, delta, session, clean):
            path.mkdir(mode=0o700)
        (config / 'cake-autorate').write_text("config globals 'globals'\n list status_columns 'route'\nconfig cake_autorate 'lab'\n option base_dl_shaper_rate_kbps '900000'\n")
        (config / 'sqm').write_text("config queue 'lab'\n option download '900000'\n")
        (config / 'cake-autorate-ui').write_text("config globals 'globals'\n")
        runtime_before = [(config / name).read_bytes() for name in ('cake-autorate', 'sqm')]
        command = uci_prefix + ['-c', str(config), '-C', str(override), '-t', str(delta)]

        def uci(*args):
            return subprocess.check_output(command + list(args), text=True)

        uci('set', 'cake-autorate.lab.base_dl_shaper_rate_kbps=12345')
        uci('set', 'sqm.lab.download=54321')
        uci('set', 'cake-autorate-ui.globals.unrelated=pending-only')
        uci('-t', str(session), 'set', 'cake-autorate.lab.base_dl_shaper_rate_kbps=11111')
        pending_before = {path: path.read_bytes() for parent in (delta, session) for path in parent.iterdir()}
        wrapper = root / 'uci-wrapper'
        wrapper.write_text('#!/bin/sh\nexec ' + shlex.join(command) + ' "$@"\n')
        wrapper.chmod(0o700)
        guard = root / 'guard'
        env = dict(os.environ, CAKE_AUTORATE_UCI_BIN=str(wrapper), CAKE_AUTORATE_LUCI_CONFIG_GUARD=str(guard), CAKE_AUTORATE_UI_CONFIG_PATH=str(config / 'cake-autorate-ui'))

        def save(*args, success=True):
            result = subprocess.run([daemon, '--status-columns', *args], env=env, capture_output=True, text=True, timeout=10)
            assert (result.returncode == 0) == success, result.stderr
            assert [(config / name).read_bytes() for name in ('cake-autorate', 'sqm')] == runtime_before
            assert {path: path.read_bytes() for parent in (delta, session) for path in parent.iterdir()} == pending_before

        save('set', 'cpu', 'cpu')
        assert uci('-t', str(clean), 'get', 'cake-autorate-ui.globals.status_columns').strip() == 'cpu'
        assert uci('-t', str(clean), 'get', 'cake-autorate-ui.globals.status_columns_set').strip() == '1'
        assert 'pending-only' not in (config / 'cake-autorate-ui').read_text()
        save('reset')
        # Libuci removes a scalar set to the empty string. The separate marker
        # still distinguishes explicit defaults from an uninitialized package.
        reset = subprocess.run(command + ['-t', str(clean), '-q', 'get', 'cake-autorate-ui.globals.status_columns'], capture_output=True, text=True)
        assert reset.returncode == 1 and reset.stdout == ''
        assert uci('-t', str(clean), 'get', 'cake-autorate-ui.globals.status_columns_set').strip() == '1'
        before_failed_write = (config / 'cake-autorate-ui').read_bytes()
        wrapper.write_text('#!/bin/sh\nfor arg do\ncase "$arg" in *.globals.status_columns=*) exit 9;; esac\ndone\nexec ' + shlex.join(command) + ' "$@"\n')
        save('set', 'route', success=False)
        assert (config / 'cake-autorate-ui').read_bytes() == before_failed_write
        wrapper.write_text('#!/bin/sh\nexec ' + shlex.join(command) + ' "$@"\n')
        with guard.open('r+') as handle:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
            save('set', 'route', success=False)
        assert (config / 'cake-autorate-ui').read_bytes() == before_failed_write
        print('REAL_UCI_STATUS_COLUMNS_ISOLATION_PASS')


if __name__ == '__main__':
    main()
