"""The `sluice` command line (SPEC §9). Works directly on SLUICE_HOME through the store."""

from __future__ import annotations

import argparse
import contextlib
import json
import os
import signal
import sys
import threading
import time
from pathlib import Path
from typing import Any

from .errors import BadRequest, NotFound, SluiceError
from .fns import RegistryError
from .runner import Runner
from .store import DEFAULT_CONFIG, Store, default_home
from .util import atomic_write_json

AUTHOR = "cli"
LOG_FIELDS = ("rev", "at", "author", "reason", "action")


def _print(obj: Any) -> None:
    print(json.dumps(obj, indent=2, ensure_ascii=False))


def _json(arg: str) -> Any:
    """A JSON argument: a path to a file, '-' for stdin, or inline JSON."""
    try:
        if arg == "-":
            return json.load(sys.stdin)
        if os.path.isfile(arg):
            return json.loads(Path(arg).read_text(encoding="utf-8"))
        return json.loads(arg)
    except (OSError, json.JSONDecodeError) as e:
        raise BadRequest(f"{arg}: not a JSON file or JSON text: {e}") from e


def _types(ports: dict[str, Any]) -> str:
    return ", ".join(f"{k}: {v if isinstance(v, str) else json.dumps(v)}"
                     for k, v in ports.items())


def cmd_init(a: argparse.Namespace) -> int:
    path = default_home() / "config.json"
    if path.exists():
        print(f"{path} already exists", file=sys.stderr)
        return 1
    atomic_write_json(path, DEFAULT_CONFIG)
    print(f"wrote {path}")
    return 0


def cmd_serve(a: argparse.Namespace, store: Store) -> int:
    import uvicorn

    from .mcp_server import build_server

    host = a.host or store.config["http"]["host"]
    port = a.port or int(store.config["http"]["port"])
    runner = Runner(store)
    store.listeners.append(runner.wake)
    thread = threading.Thread(target=runner.run_forever, name="sluice-runner", daemon=True)
    thread.start()

    class Server(uvicorn.Server):
        @contextlib.contextmanager
        def capture_signals(self):
            # Shut down on SIGINT/SIGTERM without re-raising the signal afterwards, so the
            # runner stops its fns and the process exits 0.
            sigs = (signal.SIGINT, signal.SIGTERM)
            prev = {s: signal.signal(s, self.handle_exit) for s in sigs}
            try:
                yield
            finally:
                for s, h in prev.items():
                    signal.signal(s, h)

    print(f"sluice: MCP at http://{host}:{port}/mcp", file=sys.stderr, flush=True)
    app = build_server(store).streamable_http_app(host=host)
    try:
        Server(uvicorn.Config(app, host=host, port=port, log_level="warning")).run()
    finally:
        runner.stop()
        thread.join(timeout=30)
    return 0


def cmd_loop(a: argparse.Namespace, store: Store) -> int:
    runner = Runner(store)
    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, lambda *_: runner.stop())
    runner.run_forever()
    return 0


def cmd_fn(a: argparse.Namespace, store: Store) -> int:
    if a.fn_cmd == "list":
        for name in store.registry.names():
            fn = store.registry.fns[name]
            print(f"{name:<18} ({_types(fn.raw['inputs'])}) -> ({_types(fn.raw['outputs'])})")
            if fn.doc:
                print(f"{'':<18} {fn.doc}")
    elif a.fn_cmd == "show":
        fn = store.registry.get(a.name)
        if fn is None:
            raise NotFound(f"no fn {a.name!r}")
        _print(fn.raw)
    else:
        pid = store.create_call(a.name, _json(a.inputs), AUTHOR)
        deadline = time.time() + a.wait
        res = store.call_result(pid)
        while res["status"] not in ("succeeded", "failed") and time.time() < deadline:
            time.sleep(0.2)
            res = store.call_result(pid)
        _print(res)
    return 0


def cmd_plan(a: argparse.Namespace, store: Store) -> int:
    if a.plan_cmd == "create":
        _print({"rev": store.create(a.id, _json(a.file), AUTHOR, a.reason)})
    elif a.plan_cmd == "show":
        _print(store.get(a.id))
    elif a.plan_cmd == "patch":
        _print({"rev": store.patch(a.id, a.rev, _json(a.ops), AUTHOR, a.reason)})
    else:
        for e in store.history(a.id):
            args = {k: v for k, v in e.items() if k not in LOG_FIELDS}
            what = (f"{len(e['ops'])} op(s)" if "ops" in e
                    else f"{e['action']} {json.dumps(args)}")
            print(f"rev {e['rev']:<3} {e['at']} {e['author']:<8} {what}  {e['reason']}")
    return 0


def cmd_status(a: argparse.Namespace, store: Store) -> int:
    s = store.status(a.id)
    if a.json:
        _print(s)
        return 0
    print(f"plan {a.id}  rev {s['rev']}")
    for kind in ("inputs", "outputs"):
        if s[kind]:
            print(f"  {kind}: " + ", ".join(f"{k}={json.dumps(v)}" for k, v in s[kind].items()))
    for st in s["steps"]:
        if st.get("error"):
            detail = st["error"].splitlines()[0]
        else:
            detail = json.dumps(st["outputs"]) if "outputs" in st else ""
        status = st["status"] + (" (manual)" if st["manual"] else "")
        print(f"  {st['id']:<16} {st['run']:<16} {status:<19} {detail[:70]}")
    return 0


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="sluice", description="Run typed plans of fns.")
    sub = p.add_subparsers(dest="cmd", required=True)
    sub.add_parser("init", help="write a default config.json")
    s = sub.add_parser("serve", help="runner + MCP server")
    s.add_argument("--host")
    s.add_argument("--port", type=int)
    sub.add_parser("loop", help="runner only")

    fn = sub.add_parser("fn", help="list, show and call fns").add_subparsers(
        dest="fn_cmd", required=True)
    fn.add_parser("list")
    fn.add_parser("show").add_argument("name")
    s = fn.add_parser("call", help="run one fn as a one-step plan (a runner must be running)")
    s.add_argument("name")
    s.add_argument("inputs", help="JSON inputs: inline, a file, or '-'")
    s.add_argument("--wait", type=float, default=0.0, help="seconds to wait for the result")

    plan = sub.add_parser("plan", help="create, show, patch plans").add_subparsers(
        dest="plan_cmd", required=True)
    s = plan.add_parser("create")
    s.add_argument("id")
    s.add_argument("file", help="plan JSON: a file, '-' or inline")
    s.add_argument("--reason", default="created from the CLI")
    plan.add_parser("show").add_argument("id")
    s = plan.add_parser("patch")
    s.add_argument("id")
    s.add_argument("ops", help="JSON Patch: a file, '-' or inline")
    s.add_argument("--rev", type=int, required=True)
    s.add_argument("--reason", required=True)
    plan.add_parser("history").add_argument("id")

    for name, arg, help_ in (("set-input", "name", "set a plan input"),
                             ("set-output", "step", "mark a step succeeded by hand")):
        s = sub.add_parser(name, help=help_)
        s.add_argument("plan")
        s.add_argument(arg)
        s.add_argument("value", help="JSON: inline, a file, or '-'")
        s.add_argument("--reason", default="")
    s = sub.add_parser("retry", help="set a failed or manual step back to pending")
    s.add_argument("plan")
    s.add_argument("step")
    s.add_argument("--reason", default="")
    s = sub.add_parser("status", help="inputs, outputs and step statuses of a plan")
    s.add_argument("id")
    s.add_argument("--json", action="store_true")
    return p


def main(argv: list[str] | None = None) -> int:
    a = build_parser().parse_args(argv)
    try:
        if a.cmd == "init":
            return cmd_init(a)
        store = Store()
        if a.cmd == "set-input":
            store.set_input(a.plan, a.name, _json(a.value), AUTHOR, a.reason)
        elif a.cmd == "set-output":
            store.set_output(a.plan, a.step, _json(a.value), AUTHOR, a.reason)
        elif a.cmd == "retry":
            store.retry(a.plan, a.step, AUTHOR, a.reason)
        else:
            handlers = {"serve": cmd_serve, "loop": cmd_loop, "fn": cmd_fn, "plan": cmd_plan,
                        "status": cmd_status}
            return handlers[a.cmd](a, store)
        _print({"ok": True})
        return 0
    except SluiceError as e:
        print(json.dumps(e.payload(), indent=2), file=sys.stderr)
        return 1
    except RegistryError as e:
        print("sluice: cannot load fns:\n  " + "\n  ".join(e.errors), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
