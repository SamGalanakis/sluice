"""The `sluice` command line (SPEC §10). Operates on SLUICE_HOME through the same store."""

from __future__ import annotations

import argparse
import json
import os
import signal
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Any

from . import lifecycle as L
from . import views
from .errors import BadRequest, NotFound, SluiceError
from .fns import RegistryError
from .runner import Runner
from .store import DEFAULT_CONFIG, Store, default_home
from .util import atomic_write_json

AUTHOR = "cli"
REPO_PACKS = Path(__file__).resolve().parents[2] / "packs"


def _out(obj: Any) -> None:
    print(json.dumps(obj, indent=2, ensure_ascii=False))


def _json_arg(arg: str) -> Any:
    """A JSON argument: '-' for stdin, a path to a file, or inline JSON."""
    try:
        if arg == "-":
            return json.load(sys.stdin)
        if os.path.isfile(arg):
            return json.loads(Path(arg).read_text(encoding="utf-8"))
        return json.loads(arg)
    except (OSError, json.JSONDecodeError) as e:
        raise BadRequest(f"{arg}: not a JSON file or JSON text: {e}") from e


def _brief(types: dict[str, Any]) -> str:
    return ", ".join(f"{k}: {json.dumps(v) if not isinstance(v, str) else v}"
                     for k, v in types.items())


# ---- commands ---------------------------------------------------------------------------


def cmd_init(a: argparse.Namespace) -> int:
    home = default_home()
    path = home / "config.json"
    if path.exists() and not a.force:
        print(f"{path} exists (use --force to overwrite)", file=sys.stderr)
        return 1
    packs = a.pack if a.pack else [str(p) for p in sorted(REPO_PACKS.glob("*")) if p.is_dir()]
    cfg = {**DEFAULT_CONFIG, "packs": [str(Path(p).resolve()) for p in packs]}
    atomic_write_json(path, cfg)
    print(f"wrote {path}")
    return 0


def cmd_serve(a: argparse.Namespace, store: Store) -> int:
    import uvicorn

    from .mcp_server import build_server

    http = store.config.get("http", {})
    host = a.host or http.get("host", "127.0.0.1")
    port = a.port or int(http.get("port", 7420))
    runner = Runner(store)
    runner.lock_home()
    store.listeners.append(runner.wake)
    app = build_server(store).streamable_http_app(host=host)
    thread = threading.Thread(target=runner.run_forever, name="sluice-runner", daemon=True)
    thread.start()
    print(f"sluice: MCP at http://{host}:{port}/mcp, runner on {store.home}", file=sys.stderr,
          flush=True)
    try:
        uvicorn.run(app, host=host, port=port, log_level="warning")
    finally:
        runner.stop()
        thread.join(timeout=30)
    return 0


def cmd_loop(a: argparse.Namespace, store: Store) -> int:
    runner = Runner(store)
    runner.lock_home()
    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, lambda *_: runner.stop())
    print(f"sluice: runner on {store.home}", file=sys.stderr, flush=True)
    runner.run_forever()
    return 0


def cmd_plan(a: argparse.Namespace, store: Store) -> int:
    if a.plan_cmd == "create":
        _out({"rev": store.create(a.id, _json_arg(a.file), a.author, a.reason)})
    elif a.plan_cmd == "show":
        doc = store.get(a.id)
        if a.json:
            _out(doc)
            return 0
        print(f"plan {doc['id']}  rev {doc['rev']}  {doc.get('title', '')}"
              + ("  [paused]" if doc.get("paused") else ""))
        if doc.get("resources"):
            print("resources: " + ", ".join(f"{k}={v}" for k, v in doc["resources"].items()))
        _, exp = store.expanded(a.id)
        for nid, en in exp.nodes.items():
            if en.parent is not None:
                continue
            deps = f"  <- {', '.join(en.deps)}" if en.deps else ""
            flags = "  [hold]" if en.hold else ""
            print(f"  {nid:<24} {en.fn.name:<20}{deps}{flags}")
    elif a.plan_cmd == "patch":
        _out({"rev": store.patch(a.id, a.rev, _json_arg(a.ops), a.author, a.reason)})
    elif a.plan_cmd == "history":
        for e in store.history(a.id):
            ops = e["ops"]
            what = "create" if e["rev"] == 1 else f"{len(ops)} op(s): " + ", ".join(
                f"{o.get('op')} {o.get('path')}" for o in ops[:4]) + (" ..." if len(ops) > 4
                                                                     else "")
            print(f"rev {e['rev']:<4} {e['at']}  {e['author']:<16} {e['reason']}  [{what}]")
    return 0


def _print_status(s: dict[str, Any], pid: str) -> None:
    counts = ", ".join(f"{v} {k}" for k, v in sorted(s["counts"].items()))
    print(f"plan {pid}  rev {s['rev']}  state {s['state_rev']}  ({counts})")
    print(f"  {'ID':<24} {'FN':<18} {'STATUS':<10} {'ATT':<4} {'STARTED':<21} "
          f"{'FINISHED':<21} ERROR")
    for n in s["nodes"]:
        att = "" if n["attempt"] is None else str(n["attempt"])
        err = (n.get("error") or "").splitlines()[0][:60] if n.get("error") else ""
        print(f"  {n['id']:<24} {n['fn']:<18} {n['status']:<10} {att:<4} "
              f"{n['started'] or '-':<21} {n['finished'] or '-':<21} {err}")
    print(f"  ready: {', '.join(s['ready']) or 'none'}")


def cmd_status(a: argparse.Namespace, store: Store) -> int:
    last = None
    while True:
        s = views.status(store, a.id)
        if a.json:
            if s["state_rev"] != last:
                _out(s)
        elif s["state_rev"] != last:
            _print_status(s, a.id)
        last = s["state_rev"]
        if not a.watch or all(n["status"] in L.TERMINAL for n in s["nodes"]):
            return 0
        time.sleep(1)


def cmd_dry_run(a: argparse.Namespace, store: Store) -> int:
    d = views.dry_run(store, a.id)
    if a.json:
        _out(d)
        return 0
    for action in ("start", "skip", "blocked"):
        for x in d[action]:
            print(f"{action:<8} {x['id']:<24} {x['reason']}")
    if not any(d.values()):
        print("nothing pending")
    return 0


def _print_event(e: dict[str, Any]) -> None:
    data = json.dumps(e.get("data") or {}, ensure_ascii=False)
    print(f"{e['seq']:>5} {e['at']} {e['type']:<16} {e.get('node', ''):<24} {data}")


def cmd_events(a: argparse.Namespace, store: Store) -> int:
    evs = store.events(a.id, None, a.limit)
    for e in evs:
        _print_event(e)
    seq = evs[-1]["seq"] if evs else 0
    while a.follow:
        time.sleep(0.5)
        for e in store.events(a.id, seq):
            _print_event(e)
            seq = e["seq"]
    return 0


def cmd_inbox(a: argparse.Namespace, store: Store) -> int:
    if a.inbox_cmd == "resolve":
        _out(L.inbox_resolve(store, a.item, _json_arg(a.resolution), a.author))
        return 0
    items = store.inbox_list(a.plan, open_only=not a.all)
    if a.json:
        _out(items)
        return 0
    for it in items:
        what = it.get("question") if it["kind"] == "ask" else it.get("error")
        what = (what or "").splitlines()[0][:80] if what else ""
        print(f"{it['id']:<24} {it['status']:<9} {it['kind']:<8} {it['node']:<24} {what}")
    if not items:
        print("inbox empty")
    return 0


def cmd_node(a: argparse.Namespace, store: Store) -> int:
    _out(L.node_action(store, a.plan, a.node, a.action, a.reason, a.author))
    return 0


def cmd_fn(a: argparse.Namespace, store: Store) -> int:
    reg = store.registry
    if a.fn_cmd == "list":
        for name in reg.names():
            fn = reg.fns[name]
            kind = " [composite]" if fn.composite else ""
            print(f"{name:<22} ({_brief(fn.raw.get('in', {}))}) -> "
                  f"({_brief(fn.raw.get('out', {}))}){kind}")
            if fn.description:
                print(f"{'':<22} {fn.description}")
    elif a.fn_cmd == "show":
        fn = reg.get(a.name)
        if fn is None:
            raise NotFound(f"no fn {a.name!r}")
        _out(fn.raw)
    elif a.fn_cmd == "test":
        return _fn_test(store, a.name, _json_arg(a.input), a.timeout)
    elif a.fn_cmd == "call":
        call = views.fn_call(store, a.name, _json_arg(a.input), a.author)
        res = {"call": call, "status": "pending"}
        deadline = time.time() + a.wait
        while a.wait > 0:
            res = views.fn_result(store, call)
            if views.call_settled(res) or time.time() >= deadline:
                break
            time.sleep(0.2)
        _out(res)
    elif a.fn_cmd == "result":
        _out(views.fn_result(store, a.call))
    return 0


def _fn_test(store: Store, name: str, inp: Any, timeout: float) -> int:
    """Run one fn outside any plan: a throwaway SLUICE_HOME, the real runner and launcher."""
    if store.registry.get(name) is None:
        raise NotFound(f"no fn {name!r}")
    with tempfile.TemporaryDirectory(prefix="sluice-fn-test-") as tmp:
        atomic_write_json(Path(tmp) / "config.json", {**store.config, "tick": "1s"})
        temp = Store(tmp, registry=store.registry)
        call = views.fn_call(temp, name, inp, "fn-test")
        runner = Runner(temp)
        deadline = time.time() + timeout
        res = views.fn_result(temp, call)
        while not views.call_settled(res) and time.time() < deadline:
            runner.tick()
            res = views.fn_result(temp, call)
            time.sleep(0.1)
        if res["status"] == "succeeded":
            _out(res["output"])
            return 0
        for e in temp.events(call):
            if e["type"] in ("node_retrying", "node_failed"):
                print(f"{e['type']}: {json.dumps(e['data'])}", file=sys.stderr)
        if res.get("stderr_tail"):
            print("--- stderr ---\n" + res["stderr_tail"].rstrip(), file=sys.stderr)
        if res["status"] not in L.TERMINAL:
            for e in temp.read_state(call)["nodes"].values():
                L.kill_group(e.get("pid"))
            print(f"{name}: still {res['status']} after {timeout}s", file=sys.stderr)
        else:
            print(f"{name}: {res['status']}: {res.get('error', '')}", file=sys.stderr)
        return 1


# ---- parser -----------------------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="sluice", description="Run typed plans of fns.")
    sub = p.add_subparsers(dest="cmd", required=True)

    s = sub.add_parser("init", help="write a default config.json")
    s.add_argument("--pack", action="append", help="pack dir (repeatable; default: repo packs)")
    s.add_argument("--force", action="store_true")

    s = sub.add_parser("serve", help="runner + MCP server")
    s.add_argument("--host")
    s.add_argument("--port", type=int)
    sub.add_parser("loop", help="runner only")

    plan = sub.add_parser("plan", help="create, show, patch plans").add_subparsers(
        dest="plan_cmd", required=True)
    s = plan.add_parser("create")
    s.add_argument("id")
    s.add_argument("file", help="plan JSON file, '-' or inline JSON")
    s.add_argument("--reason", default="created from the CLI")
    s.add_argument("--author", default=AUTHOR)
    s = plan.add_parser("show")
    s.add_argument("id")
    s.add_argument("--json", action="store_true")
    s = plan.add_parser("patch")
    s.add_argument("id")
    s.add_argument("ops", help="JSON Patch file, '-' or inline JSON")
    s.add_argument("--rev", type=int, required=True)
    s.add_argument("--reason", required=True)
    s.add_argument("--author", default=AUTHOR)
    s = plan.add_parser("history")
    s.add_argument("id")

    s = sub.add_parser("status", help="node statuses of a plan")
    s.add_argument("id")
    s.add_argument("--watch", action="store_true", help="refresh until every node is terminal")
    s.add_argument("--json", action="store_true")

    s = sub.add_parser("dry-run", help="what the next tick would do")
    s.add_argument("id")
    s.add_argument("--json", action="store_true")

    s = sub.add_parser("events", help="the event stream of a plan")
    s.add_argument("id")
    s.add_argument("--follow", action="store_true")
    s.add_argument("--limit", type=int, default=None)

    s = sub.add_parser("inbox", help="items waiting for a decision")
    s.add_argument("--plan")
    s.add_argument("--all", action="store_true", help="include resolved items")
    s.add_argument("--json", action="store_true")
    isub = s.add_subparsers(dest="inbox_cmd")
    r = isub.add_parser("resolve")
    r.add_argument("item")
    r.add_argument("resolution", help='{"answer": ...} or {"action": "retry"|"skip"|"ack"}')
    r.add_argument("--author", default=AUTHOR)

    s = sub.add_parser("node", help="operator actions")
    s.add_argument("action", choices=["retry", "skip", "cancel"])
    s.add_argument("plan")
    s.add_argument("node")
    s.add_argument("--reason", default="operator action from the CLI")
    s.add_argument("--author", default=AUTHOR)

    fn = sub.add_parser("fn", help="list, show, test and call fns").add_subparsers(
        dest="fn_cmd", required=True)
    fn.add_parser("list")
    s = fn.add_parser("show")
    s.add_argument("name")
    s = fn.add_parser("test", help="run one fn outside any plan and print its output")
    s.add_argument("name")
    s.add_argument("input", help="input JSON file, '-' or inline JSON")
    s.add_argument("--timeout", type=float, default=3600.0)
    s = fn.add_parser("call", help="run one fn as an ad-hoc plan of this home")
    s.add_argument("name")
    s.add_argument("input", help="input JSON file, '-' or inline JSON")
    s.add_argument("--wait", type=float, default=0.0, help="seconds to wait for the result")
    s.add_argument("--author", default=AUTHOR)
    s = fn.add_parser("result")
    s.add_argument("call")
    return p


def main(argv: list[str] | None = None) -> int:
    a = build_parser().parse_args(argv)
    try:
        if a.cmd == "init":
            return cmd_init(a)
        store = Store()
        handler = {"serve": cmd_serve, "loop": cmd_loop, "plan": cmd_plan, "status": cmd_status,
                   "dry-run": cmd_dry_run, "events": cmd_events, "inbox": cmd_inbox,
                   "node": cmd_node, "fn": cmd_fn}[a.cmd]
        return handler(a, store)
    except SluiceError as e:
        print(json.dumps(e.payload(), indent=2), file=sys.stderr)
        return 1
    except RegistryError as e:
        print("sluice: cannot load fns:\n  " + "\n  ".join(e.errors), file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    sys.exit(main())
