"""Project logic tests. All external services and agents are fakes."""
import contextlib
import importlib.util
import json
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest
from sluice_fn import AgentFailure, Rejected, Transient

ROOT = Path(__file__).resolve().parent
FAKE_CLI = ROOT.parent / 'crates/sluice-runtime/tests/fixtures/staging/fake_cli.py'


def load(project, name):
    path = ROOT / 'projects' / project / 'fns' / name / 'main.py'
    spec = importlib.util.spec_from_file_location(project + '_' + name.replace('.', '_'), path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.fixture
def fake_cli(tmp_path, monkeypatch):
    bins = tmp_path / 'bin'
    bins.mkdir()
    source = FAKE_CLI.read_text().replace('#!/usr/bin/env python3', '#!' + sys.executable)
    for name in ('kiln', 'git', 'gh', 'linear', 'bash'):
        path = bins / name
        path.write_text(source)
        path.chmod(0o755)
    trace = tmp_path / 'trace.jsonl'
    monkeypatch.setenv('PATH', str(bins))
    monkeypatch.setenv('SLUICE_HOST_PATH', str(bins))
    monkeypatch.setenv('STAGING_TRACE', str(trace))
    import shutil
    import subprocess
    real_popen = subprocess.Popen

    def guarded_popen(argv, *args, **kwargs):
        import os
        env = kwargs.get('env') or os.environ
        executable = Path(argv[0])
        if executable.name in ('kiln', 'git', 'gh', 'linear', 'bash'):
            resolved = executable if executable.is_absolute() else Path(
                shutil.which(str(executable), path=env.get('PATH')) or '/missing')
            assert resolved.parent == bins, f'offline command escaped fixture PATH: {argv}'
        elif executable == Path(sys.executable):
            assert env.get('SLUICE_HOST_PATH') == str(bins)
        return real_popen(argv, *args, **kwargs)

    monkeypatch.setattr(subprocess, 'Popen', guarded_popen)

    def configure(responses):
        monkeypatch.setenv('STAGING_RESPONSES', json.dumps(responses))

    def calls():
        return [json.loads(line) for line in trace.read_text().splitlines()] if trace.exists() else []

    return configure, calls


def reply(argv, stdout='', code=0, **extra):
    return {'argv': argv, 'stdout': stdout, 'code': code, **extra}


@pytest.mark.parametrize('project', ['lash', 'figments'])
def test_linear_payloads_and_partial_close(project, tmp_path, fake_cli):
    configure, calls = fake_cli
    ctx = SimpleNamespace(run_dir=tmp_path)
    create, comment, close = [load(project, 'linear.' + op) for op in ('create', 'comment', 'close')]
    cli = str(tmp_path / 'bin/linear') if project == 'lash' else 'linear'
    configure([
        reply(['linear', 'issue', 'create', '--no-interactive', '--title', 'Fix it', '--team',
               'ABC', '--description-file', str(tmp_path / 'description.md'), '--parent',
               'ABC-1', '--project', 'Runtime', '--state', 'Todo', '--label', 'bug'],
              'ABC-2 https://linear.app/team/issue/ABC-2\n'),
        reply(['linear', 'issue', 'comment', 'add', 'ABC-2', '--body-file', str(tmp_path / 'comment.md')]),
        reply(['linear', 'issue', 'comment', 'add', 'ABC-2', '--body-file', str(tmp_path / 'evidence.md')]),
        reply(['linear', 'issue', 'update', 'ABC-2', '--state', 'completed']),
    ])
    assert create.main({'title': 'Fix it', 'parent': 'ABC-1', 'description': 'Cause',
                        'project': 'Runtime', 'state': 'Todo', 'labels': ['bug']}, ctx) == {
                            'id': 'ABC-2', 'url': 'https://linear.app/team/issue/ABC-2'}
    assert (tmp_path / 'description.md').read_text() == 'Cause'
    assert comment.main({'issue': 'ABC-2', 'body': 'Evidence'}, ctx) == {'ok': True}
    assert (tmp_path / 'comment.md').read_text() == 'Evidence'
    if project == 'lash':
        assert close.main({'issue': 'ABC-2', 'message': 'Part of ABC-2', 'evidence': 'Partial'}, ctx)['closed'] is False
        assert not any('update' in c['argv'] for c in calls())
    assert close.main({'issue': 'ABC-2', 'message': 'Closes ABC-2', 'evidence': 'Done'}, ctx)['ok'] is True
    assert (tmp_path / 'evidence.md').read_text() == 'Done'
    assert calls()[-1]['argv'] == ['linear', 'issue', 'update', 'ABC-2', '--state', 'completed']
    assert cli


@pytest.mark.parametrize('project', ['lash', 'figments'])
def test_fork_keeps_kiln_options_and_uses_project_dir(project, tmp_path, fake_cli):
    configure, calls = fake_cli
    module = load(project, project + '.fork')
    fork = tmp_path / 'fork'
    fork.mkdir()
    ctx = SimpleNamespace(project_dir=tmp_path)
    inp = {'name': 'lane', 'review': True, 'base': 'origin/review'}
    options = ['--no-build'] + (['--branch', 'review'] if project == 'figments' else [])
    configure([
        reply(['kiln', 'fork', *options, project, 'lane'], 'diagnostic\n' + str(fork) + '\n'),
        reply(['git', '-C', str(fork), 'checkout', '-q', '--detach', 'origin/review']),
        reply(['git', '-C', str(fork), 'rev-parse', 'HEAD'], 'abcdef\n'),
    ])
    out = module.main(inp, ctx)
    assert out['path'] == str(fork) and out['head'] == 'abcdef'
    assert calls()[0]['cwd'] == str(tmp_path)


@pytest.mark.parametrize('project', ['lash', 'figments'])
def test_fork_rm_preserves_refusals_and_safe_removal(project, tmp_path, fake_cli, monkeypatch):
    configure, calls = fake_cli
    module = load(project, project + '.fork_rm')
    fork = tmp_path / 'fork'
    fork.mkdir()
    ctx = SimpleNamespace(project_dir=tmp_path, log=lambda _: None)
    inp = {'name': 'lane', 'path': str(fork)}
    git = ['git', '-C', str(fork)]
    if project == 'lash':
        monkeypatch.setattr(module, 'ARCHIVE', tmp_path / 'archive')
        status = [*git, 'status', '--porcelain', '--untracked-files=no']
        rest = [reply([*git, 'ls-files', '--others', '--exclude-standard', '--directory'], 'note.txt\n'),
                reply([*git, 'fetch', '-q', 'origin', 'main']),
                reply([*git, 'merge-base', '--is-ancestor', 'HEAD', 'origin/main'])]
        (fork / 'note.txt').write_text('Evidence')
    else:
        status = [*git, 'status', '--porcelain']
        rest = [reply([*git, 'fetch', '-q', 'origin']),
                reply([*git, 'branch', '-r', '--contains', 'HEAD'], 'origin/main\n')]
    configure([reply(status, ' M changed.rs\n'), *rest])
    with pytest.raises(RuntimeError, match='(tracked changes|uncommitted changes)'):
        module.main(inp, ctx)
    assert not any(c['argv'][0] == 'kiln' for c in calls())
    configure([reply(status), *rest, reply(['kiln', 'rm', project, 'lane'])])
    out = module.main(inp, ctx)
    assert out['removed'] is True and calls()[-1]['cwd'] == str(tmp_path)
    if project == 'lash':
        assert (tmp_path / 'archive/lane/note.txt').read_text() == 'Evidence'
        assert out['discarded'] == ['note.txt']


@pytest.mark.parametrize('project', ['lash', 'figments'])
def test_worker_composition_submission_head_and_summary(project, tmp_path, fake_cli):
    configure, _ = fake_cli
    worker = load(project, project + '.worker')
    configure([reply(['git', '-C', str(tmp_path), 'rev-parse', 'HEAD'], 'abcdef123\n')])
    requests = []
    def builtin(name, inp):
        assert name == 'agent.run'
        requests.append(inp)
        return {'session': 'session', 'report': ' '.join(str(i) for i in range(130))}
    ctx = SimpleNamespace(run_dir=tmp_path, header=lambda text: text, builtin=builtin, submission=lambda: {'head_sha': 'abcdef'})
    inp = {'engine': 'codex', 'cwd': str(tmp_path), 'spec': 'Do it.'}
    out = worker.main(inp, ctx)
    assert len(out['summary'].split()) == 120 and out['final'] == out['summary']
    assert out['session'] == 'session'
    assert requests[0]['spec'].endswith('Do it.')
    assert 'env.sh' in requests[0]['spec'] and str(tmp_path / 'summary.txt') in requests[0]['spec']
    assert requests[0]['report_path'] == str(tmp_path / 'summary.txt')
    ctx.submission = lambda: {'head_sha': 'wrong'}
    with pytest.raises(RuntimeError, match='reported head_sha wrong'):
        worker.main(inp, ctx)


def test_lash_wall_cap_continues_once_and_other_failures_propagate(tmp_path):
    worker = load('lash', 'lash.worker')
    inp = {'engine': 'codex', 'cwd': str(tmp_path), 'spec': 'Do it.'}
    calls = []
    def builtin(name, request):
        calls.append(request)
        if len(calls) == 1:
            raise AgentFailure('WallCap', 'Stopped', 'saved-session')
        return {'session': 'saved-session', 'report': 'Ready'}
    ctx = SimpleNamespace(run_dir=tmp_path, header=lambda text: text, builtin=builtin, submission=dict)
    assert worker.main(inp, ctx)['session'] == 'saved-session'
    assert len(calls) == 2 and calls[1]['session'] == 'saved-session'
    assert calls[1]['spec'].startswith(worker.WALL_CAP_CONTINUE)
    for failure in [AgentFailure('WallCap', 'Stopped'), AgentFailure('Stall', 'Stopped', 's'),
                    Transient('retry in the helper')]:
        def fail(*args, failure=failure):
            raise failure
        ctx.builtin = fail
        with pytest.raises(type(failure)):
            worker.main(inp, ctx)
    calls.clear()
    def caps(*args):
        calls.append(args)
        raise AgentFailure('WallCap', 'Stopped', 's')
    ctx.builtin = caps
    with pytest.raises(AgentFailure):
        worker.main(inp, ctx)
    assert len(calls) == 2


def test_decide_posts_owner_note_on_thread_and_returns_message_id(tmp_path):
    module = load('lash', 'lash.decide')
    requests = []
    def tool(name, body):
        requests.append((name, body))
        return {'id': 42}
    out = module.main({'title': 'Choose it', 'decision': 'Use it', 'alternative': 'Wait',
                       'reversible': 'Yes', 'refs': 'file:1', 'thread': 'arc-runtime'},
                      SimpleNamespace(tool=tool))
    assert out == {'id': 42}
    name, note = requests[0]
    assert name == 'message_post' and note['to'] == 'owner' and note['needs_reply'] is False
    assert note['thread'] == 'arc-runtime' and note['body'] == 'Use it\n\nAlternative: Wait\n\nReversible: Yes\n\nEvidence: file:1'
    assert list(tmp_path.iterdir()) == []


def test_lane_capacity_keeps_measured_load_policy(monkeypatch):
    module = load('lash', 'lash.lane_capacity')
    for load_value, expected in [(48, 56), (48.01, 0)]:
        monkeypatch.setattr(module.os, 'getloadavg', lambda value=load_value: (value, 0, 0))
        assert module.main({}, None) == {'capacity': expected}


def test_on_main_sha_and_grep_use_canned_git(tmp_path, fake_cli):
    configure, _ = fake_cli
    module = load('lash', 'lash.on_main')
    git = ['git', '-C', str(tmp_path)]
    configure([reply([*git, 'fetch', '-q', 'origin', 'main']),
               reply([*git, 'merge-base', '--is-ancestor', 'abc', 'origin/main']),
               reply([*git, 'rev-parse', 'abc'], 'abcdef\n'),
               reply([*git, 'show', '-s', '--format=%cI', 'abcdef'], '2026-10-03T00:00:00Z\n'),
               reply([*git, 'log', 'origin/main', '-1', '--format=%H', '--grep=FIG-1'], 'abc\n')])
    ctx = SimpleNamespace(log=lambda _: None)
    for key, val in [('sha', 'abc'), ('grep', 'FIG-1')]:
        assert module.main({'repo': str(tmp_path), key: val}, ctx) == {
            'sha': 'abcdef', 'at': '2026-10-03T00:00:00Z'}


def test_dev_test_dry_run_selection_and_gates(tmp_path, fake_cli):
    configure, _ = fake_cli
    module = load('lash', 'lash.dev_test')
    plan = {'commands': [['kiln', 'test', '//one']], 'selection': 'focused', 'changed_files': ['a.rs']}
    script = '. ./env.sh && python3 scripts/dev-test.py --dependents --base base --dry-run'
    configure([reply(['bash', '-c', script], json.dumps(plan))])
    out = module.main({'fork': str(tmp_path), 'dry_run': True, 'dependents': True,
                       'base': 'base', 'require_ok': True, 'require_tests': True}, None)
    assert out['ok'] and out['tested'] and out['commands'] == ['kiln test //one']
    configure([reply(['bash', '-c', '. ./env.sh && python3 scripts/dev-test.py --dry-run'],
                     json.dumps({**plan, 'commands': []}))])
    with pytest.raises(RuntimeError, match='selected no tests'):
        module.main({'fork': str(tmp_path), 'dry_run': True, 'require_tests': True}, None)


def test_dev_test_receipt_must_be_fresh_and_checkout_unchanged(tmp_path, monkeypatch):
    module = load('lash', 'lash.dev_test')
    receipt = tmp_path / 'latest.json'
    monkeypatch.setattr(module, 'receipt_path', lambda _: receipt)
    monkeypatch.setattr(module.time, 'time_ns', lambda: 100)
    monkeypatch.setattr(module, 'stream', lambda *a, **kw: SimpleNamespace(returncode=0, stdout='', stderr=''))
    with pytest.raises(RuntimeError, match='no receipt'):
        module.main({'fork': str(tmp_path)}, None)
    plan = {'commands': [['kiln', 'check', '//one']], 'selection': 'focused', 'changed_files': []}
    receipt.write_text(json.dumps({'started_ns': 99, 'plan': plan, 'exit_code': 0, 'inputs_unchanged': True}))
    with pytest.raises(RuntimeError, match='no receipt'):
        module.main({'fork': str(tmp_path)}, None)
    receipt.write_text(json.dumps({'started_ns': 101, 'plan': plan, 'exit_code': 0, 'inputs_unchanged': False}))
    assert module.main({'fork': str(tmp_path)}, None)['ok'] is False
    with pytest.raises(RuntimeError, match='checkout changed'):
        module.main({'fork': str(tmp_path), 'require_ok': True}, None)


def test_land_ready_false_registers_one_refusal_before_commands(tmp_path, monkeypatch):
    module = load('lash', 'lash.land')
    monkeypatch.setattr(module, 'sh', lambda *a, **kw: pytest.fail('ready=false must not execute a command'))
    actions = []
    ctx = SimpleNamespace(step='renamed-unit-land', retry_on_failure=lambda *a: actions.append(a))
    with pytest.raises(Rejected, match='ready=false'):
        module.main({'fork': str(tmp_path), 'ready': False, 'unresolved': 'Needs fixes'}, ctx)
    assert len(actions) == 1 and actions[0][0] == 'renamed-unit-work'
    assert 'Needs fixes' in actions[0][1]
    actions.clear()
    with pytest.raises(Rejected):
        module.main({'fork': str(tmp_path), 'ready': False, 'work_step': 'explicit'}, ctx)
    assert actions[0][0] == 'explicit'


def test_land_releases_lease_for_check_and_reacquires_for_push(monkeypatch):
    module = load('lash', 'lash.land')
    held, events = False, []
    @contextlib.contextmanager
    def acquire(name, amount):
        nonlocal held
        assert name == 'land' and amount == 1 and not held
        held = True
        events.append('acquire')
        try:
            yield
        finally:
            held = False
            events.append('release')
    ctx = SimpleNamespace(acquire=acquire)
    def sh(argv, **kw):
        if 'fetch' in argv or 'push' in argv:
            assert held
        value = 'base' if 'merge-base' in argv else '1' if 'rev-list' in argv else 'main'
        return SimpleNamespace(stdout=value, stderr='', returncode=0)
    monkeypatch.setattr(module, 'sh', sh)
    monkeypatch.setattr(module, 'net_sh', sh)
    monkeypatch.setattr(module, 'names', lambda fork, rng: {'a.rs'} if rng == 'origin/main...HEAD' or rng == 'base..main' else set())
    monkeypatch.setattr(module, 'rebase', lambda *a: False)
    def kiln(*args):
        assert not held
        events.append('check')
        return SimpleNamespace(returncode=0, stdout='BUILD SUCCEEDED')
    monkeypatch.setattr(module, 'kiln', kiln)
    module.land('fork', lambda _: None, ctx, {})
    assert events == ['acquire', 'release', 'check', 'acquire', 'release']


def test_land_failed_check_is_rejected_with_feedback_after_release(monkeypatch):
    module = load('lash', 'lash.land')
    events = []
    @contextlib.contextmanager
    def acquire(*args):
        events.append('acquire')
        yield
        events.append('release')
    ctx = SimpleNamespace(step='lane-land', acquire=acquire,
                          retry_on_failure=lambda *a: events.append(a))
    monkeypatch.setattr(module, 'sh', lambda *a, **kw: SimpleNamespace(stdout='1', stderr='', returncode=0))
    monkeypatch.setattr(module, 'net_sh', module.sh)
    monkeypatch.setattr(module, 'names', lambda *a: set())
    monkeypatch.setattr(module, 'rebase', lambda *a: True)
    monkeypatch.setattr(module, 'kiln', lambda *a: SimpleNamespace(returncode=1, stdout='error: broken'))
    with pytest.raises(Rejected, match='kiln check failed'):
        module.land('fork', lambda _: None, ctx, {})
    assert events[:2] == ['acquire', 'release'] and events[2][0] == 'lane-work'
    assert 'error: broken' in events[2][1]


PROJECT_FNS = [
    (project, path.parent.name)
    for project in ('lash', 'figments')
    for path in sorted((ROOT / 'projects' / project / 'fns').glob('*/fn.json'))
]


@pytest.mark.parametrize(('project', 'name'), PROJECT_FNS)
def test_real_helper_envelope_for_each_staged_fn(project, name, tmp_path, fake_cli, monkeypatch):
    """Runs automatically when p4-01 lands. The logic tests above never need that dependency."""
    import os
    import subprocess

    import conftest

    if not conftest.REAL_HELPER:
        pytest.skip('p4-01 python/sluice_fn is absent; real helper envelope gate is pending')
    configure, _ = fake_cli
    run_dir, fork = tmp_path / 'run', tmp_path / 'fork'
    run_dir.mkdir()
    fork.mkdir()
    callback = tmp_path / 'callback'
    callback.write_text((FAKE_CLI.with_name('fake_callback.py')).read_text().replace(
        '#!/usr/bin/env python3', '#!' + sys.executable))
    callback.chmod(0o755)
    callback_trace = tmp_path / 'callback.jsonl'
    monkeypatch.setenv('STAGING_CALLBACK_TRACE', str(callback_trace))
    project_id = '019a0000-0000-7000-8000-000000000001'
    monkeypatch.setenv('SLUICE_PROJECT_ID', project_id)
    monkeypatch.setenv('SLUICE_HOST_PATH', str(tmp_path / 'bin'))
    monkeypatch.setenv('SLUICE_BACKOFF', '0')
    git = ['git', '-C', str(fork)]
    responses = [reply([*git, 'rev-parse', 'HEAD'], 'abc123\n')]
    expected = None
    if name.endswith('.worker'):
        inp = {'cwd': str(fork), 'spec': 'Do it.', 'engine': 'codex'}
        expected = {'session': 'session', 'summary': 'Ready with offline evidence.',
                    'final': 'Ready with offline evidence.'}
    elif name.endswith('.fork_rm'):
        inp = {'path': str(tmp_path / 'absent'), 'name': 'lane'}
        expected = {'removed': False, **({'discarded': []} if project == 'lash' else {})}
    elif name.endswith('.fork'):
        inp = {'name': 'envelope-fixture', 'review': True}
        responses.append(reply(['kiln', 'fork', '--no-build', project, 'envelope-fixture'], str(fork) + '\n'))
        expected = {'path': str(fork), 'head': 'abc123'}
        if project == 'figments':
            expected.update(branch=None, reused=False)
    elif name == 'lash.land':
        inp = {'fork': str(fork), 'ready': False, 'work_step': 'lane-work'}
    elif name == 'lash.decide':
        inp = {'title': 'Chosen', 'decision': 'Use it', 'thread': 'arc-fixture'}
        expected = {'id': 42}
    elif name == 'lash.lane_capacity':
        inp = {}
    elif name == 'lash.on_main':
        inp = {'repo': str(fork), 'sha': 'abc123'}
        responses += [reply([*git, 'fetch', '-q', 'origin', 'main']),
                      reply([*git, 'merge-base', '--is-ancestor', 'abc123', 'origin/main']),
                      reply([*git, 'rev-parse', 'abc123'], 'abc123\n'),
                      reply([*git, 'show', '-s', '--format=%cI', 'abc123'], '2026-10-03T00:00:00Z\n')]
        expected = {'sha': 'abc123', 'at': '2026-10-03T00:00:00Z'}
    elif name == 'lash.main_red':
        inp = {'repo': str(fork)}
        fields = 'databaseId,headSha,status,conclusion,workflowName,event,url'
        run = {'databaseId': 1, 'headSha': 'abc123', 'status': 'completed', 'conclusion': 'success',
               'workflowName': 'CI', 'event': 'workflow_dispatch', 'url': 'fixture'}
        responses += [reply(['gh', 'run', 'list', '--branch', 'main', '--workflow', 'CI',
                             '--event', 'workflow_dispatch', '--limit', '30', '--json', fields],
                            json.dumps([run])),
                      reply(['gh', 'run', 'view', '1', '--json', 'jobs'], '{"jobs":[]}')]
        expected = {'run_id': 1, 'sha': 'abc123', 'conclusion': 'success', 'red': False,
                    'url': 'fixture', 'failed_jobs': [], 'failed_job_ids': [], 'failed_tests': []}
    elif name == 'lash.dev_test':
        inp = {'fork': str(fork), 'dry_run': True}
        plan = {'selection': 'empty', 'commands': [], 'changed_files': []}
        responses.append(reply(['bash', '-c', '. ./env.sh && python3 scripts/dev-test.py --dry-run'], json.dumps(plan)))
        expected = {'ok': True, 'code': 0, 'selection': 'empty', 'commands': [], 'tested': False,
                    'changed_files': [], 'tail': json.dumps(plan)}
    elif name == 'linear.create':
        inp = {'title': 'Task', 'description': 'Details', 'team': 'FIG'}
        responses.append(reply(['linear', 'issue', 'create', '--no-interactive', '--title', 'Task',
                                '--team', 'FIG', '--description-file', str(run_dir / 'description.md')],
                               'FIG-1 https://linear.app/fixture\n'))
        expected = {'id': 'FIG-1', 'url': 'https://linear.app/fixture'}
    elif name == 'linear.comment':
        inp = {'issue': 'FIG-1', 'body': 'Evidence'}
        responses.append(reply(['linear', 'issue', 'comment', 'add', 'FIG-1', '--body-file', str(run_dir / 'comment.md')]))
        expected = {'ok': True}
    elif name == 'linear.close':
        inp = {'issue': 'FIG-1'}
        responses.append(reply(['linear', 'issue', 'update', 'FIG-1', '--state', 'completed']))
        expected = {'ok': True, **({'closed': True} if project == 'lash' else {})}
    else:
        raise AssertionError(f'missing envelope fixture for {name}')
    configure(responses)
    path = ROOT / 'projects' / project / 'fns' / name / 'main.py'
    context = {'project_id': project_id, 'project': 'renamed-' + project, 'step': 'lane-land',
               'run_id': '019a0000-0000-7000-8000-000000000002',
               'attempt_id': '019a0000-0000-7000-8000-000000000003',
               'invocation_id': '019a0000-0000-7000-8000-000000000004',
               'run_dir': str(run_dir), 'home': str(tmp_path / 'home'),
               'project_dir': str(tmp_path), 'fn_dir': str(path.parent), 'bin': str(callback),
               'outputs': {'proof': {'type': 'string'}}, 'run_capability': 'fixture-capability'}
    env = dict(os.environ)
    helper_path = os.environ.get('STAGING_HELPER_ROOT', str(ROOT.parent / 'python'))
    env['PYTHONPATH'] = helper_path
    result = subprocess.run([sys.executable, str(path)], input=json.dumps(
        {'protocol': 1, 'inputs': inp, 'context': context}), env=env,
        capture_output=True, text=True, check=False, timeout=20)
    answer = json.loads(result.stdout)
    if name == 'lash.land':
        assert result.returncode == 1 and answer['error']['kind'] == 'rejected', result.stderr
        requests = [json.loads(line) for line in callback_trace.read_text().splitlines()]
        assert requests[0]['command'] == 'retry_on_failure'
    else:
        assert result.returncode == 0 and answer['ok'] is True, result.stderr
        if name == 'lash.lane_capacity':
            assert answer['outputs']['capacity'] in (0, 56)
        else:
            assert answer['outputs'] == expected
