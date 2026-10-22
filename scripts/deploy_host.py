"""Run shipping work with host capacity, independent of an agent's containment."""
import os
import pathlib
import pwd
import re
import signal
import subprocess
import uuid


def enter(script, arguments):
    # An environment flag alone must not let a caller accidentally retain its lane's cap.
    if (os.environ.get('SLUICE_TEST') == '1'
            and os.environ.get('SLUICE_DEPLOY_PROCESS_FIXTURE') == '1'
            and pathlib.Path(script).name == 'deploy'):
        # These tests own persistent plain child services by PID, rather than systemd.
        return
    unit = os.environ.get('SLUICE_HOST_UNIT', '')
    cgroup = pathlib.Path('/proc/self/cgroup').read_text().splitlines()
    if (re.fullmatch(r'sluice-(?:test-)?deploy-host-[0-9a-f]{32}\.service', unit)
            and any(line.endswith('/app.slice/' + unit) for line in cgroup)):
        return

    account = pathlib.Path(pwd.getpwuid(os.getuid()).pw_dir)
    testing = os.environ.get('SLUICE_TEST') == '1'
    env = dict(HOME=str(account), LANG='C.UTF-8', LC_ALL='C.UTF-8',
               PATH=':'.join(map(str, [account / '.cargo/bin', account / '.local/bin',
                                      account / 'bin', '/usr/local/bin', '/usr/bin', '/bin'])))
    for key in ('SLUICE_HOME', 'SLUICE_INSTALL_DIR', 'SLUICE_DEPLOY_PORT',
                'SLUICE_DEPLOY_READY_SECONDS', 'SLUICE_BUILD_GIT_SHA', 'SLUICE_TEST'):
        if key in os.environ:
            env[key] = os.environ[key]
    if testing:
        # Packaging tests deliberately substitute cargo and a home. Production never takes
        # PATH, runtime knobs, fixture commands or compiler settings from the calling lane.
        for key in ('HOME', 'PATH', 'SLUICE_FIXTURE',
                    'SLUICE_DEPLOY_PROCESS_FIXTURE'):
            if key in os.environ:
                env[key] = os.environ[key]
        env['TOKIO_WORKER_THREADS'] = '2'
        env.update((k, v) for k, v in os.environ.items() if k.startswith('SLUICE_TEST_'))
    unit = 'sluice-' + ('test-' if testing else '') + 'deploy-host-' + uuid.uuid4().hex + '.service'
    env['SLUICE_HOST_UNIT'] = unit
    # The caller may deliberately have no runtime dir or a dead bus. Only this launcher
    # connects to the real manager; compat-check's candidate still gets its dead bus.
    runtime = '/run/user/%d' % os.getuid()
    env.update(XDG_RUNTIME_DIR=runtime, DBUS_SESSION_BUS_ADDRESS='unix:path=' + runtime + '/bus')
    manager_env = env.copy()
    command = ['/usr/bin/systemd-run', '--user', '--quiet', '--wait', '--pipe', '--collect',
               '--service-type=exec', '--slice=app.slice', '--unit=' + unit,
               '--property=KillMode=mixed', '--property=TimeoutStopSec=30',
               '--working-directory=' + os.getcwd(), '--', '/usr/bin/env', '-i',
               *[k + '=' + v for k, v in env.items()], '/bin/bash', str(script), *arguments]
    child = subprocess.Popen(command, env=manager_env, stdin=subprocess.DEVNULL)

    def interrupted(signum, frame):
        # Stop the Python script first so its PID cleanup and scratch removal run, then
        # let systemd reap anything it missed. Do not leave a detached deploy running.
        subprocess.run(['/usr/bin/systemctl', '--user', 'stop', unit], env=manager_env,
                       stdin=subprocess.DEVNULL, check=False)
        child.wait()
        raise SystemExit(128 + signum)

    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    raise SystemExit(child.wait())
