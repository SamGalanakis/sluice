"""inline.bash and inline.python: code given as a string, run by the runner like any fn."""

from tests.conftest import create, settle


def d(x):
    return {"default": x}


def bash(script, outputs=None, **extra_in):
    step = {"run": "inline.bash", "in": {"script": d(script), **extra_in}}
    return {**step, "outputs": outputs} if outputs else step


def python(code, outputs=None, **extra_in):
    step = {"run": "inline.python", "in": {"code": d(code), **extra_in}}
    return {**step, "outputs": outputs} if outputs else step


def test_bash_sees_extra_inputs_as_env_and_returns_what_it_printed(store, runner, tmp_path):
    create(store, "p", {
        "a": bash('echo "$name-$count-$my_list"; echo oops >&2; pwd',
                  name=d("x y"), count=d(3), **{"my-list": d([1, 2])}, cwd=d(str(tmp_path))),
        "b": bash("echo hi | false; echo unreachable"),
        "c": bash("exit 7", check=d(False)),
    })
    steps = settle(runner, store, "p")
    out = steps["a"]["outputs"]
    assert out["stdout"] == f"x y-3-[1, 2]\n{tmp_path}\n" and out["stderr"] == "oops\n"
    assert out["code"] == 0
    assert steps["b"]["status"] == "failed" and "exit code" in steps["b"]["error"]
    lines = steps["b"]["error"].splitlines()
    assert "unreachable" not in lines  # pipefail and errexit stopped it before the echo
    assert steps["c"]["status"] == "succeeded" and steps["c"]["outputs"]["code"] == 7


def test_bash_declared_outputs_come_from_the_out_file(store, runner):
    create(store, "p", {
        "a": bash('printf \'{"sha": "abc", "n": 2, "other": 1}\' > "$OUT"',
                  {"sha": "string", "n": "int"}),
        "b": {"run": "core.echo", "in": {"value": {"source": "a/n"}}},
        "none": bash("true", {"sha": "string"}),
        "bad": bash('echo \'{"sha": 5}\' > "$OUT"', {"sha": "string"}),
    })
    steps = settle(runner, store, "p")
    assert (steps["a"]["outputs"]["sha"], steps["a"]["outputs"]["n"]) == ("abc", 2)
    assert "other" not in steps["a"]["outputs"]
    assert steps["b"]["outputs"] == {"value": 2}
    assert steps["none"]["status"] == "failed" and '"$OUT"' in steps["none"]["error"]
    assert steps["bad"]["status"] == "failed"
    assert "sha: expected string, got 5" in steps["bad"]["error"]


def test_python_sees_inputs_sets_out_and_its_prints_are_stdout(store, runner, tmp_path):
    create(store, "p", {
        "a": python("import os\nprint('cwd', os.getcwd())\nout = [n * 2 for n in nums]",
                    nums=d([1, 2, 3]), cwd=d(str(tmp_path))),
        "b": python("out = {'total': sum(inp['nums']), 'label': f'{who}!'}",
                    {"total": "int", "label": "string"}, nums=d([4, 5]), who=d("sam")),
        "c": {"run": "core.echo", "in": {"value": {"source": "b/total"}}},
        "boom": python("raise ValueError('nope')"),
        "short": python("out = 3", {"total": "int"}),
    })
    steps = settle(runner, store, "p")
    assert steps["a"]["outputs"] == {"value": [2, 4, 6], "stdout": f"cwd {tmp_path}\n"}
    assert (steps["b"]["outputs"]["total"], steps["b"]["outputs"]["label"]) == (9, "sam!")
    assert steps["c"]["outputs"] == {"value": 9}
    assert steps["boom"]["status"] == "failed" and "ValueError: nope" in steps["boom"]["error"]
    assert steps["short"]["status"] == "failed" and "set `out` to a dict" in steps["short"]["error"]
