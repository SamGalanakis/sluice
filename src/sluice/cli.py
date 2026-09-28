"""The `sluice` command line (SPEC §9): `serve`, `loop`, `tool` to call any MCP tool
in-process through the same server object `serve` exposes, `watch` to follow a log and
`next` for the one record an orchestrator acts on."""

from __future__ import annotations

import argparse
import contextlib
import ipaddress
import json
import signal
import sys
import threading
from pathlib import Path
from typing import Any

from . import db
from .errors import BadRequest, SluiceError
from .runner import Runner
from .store import DEFAULT_CONFIG, Store, default_home
from .util import atomic_write_json, atomic_write_text

# `serve` and `loop` exit 0 on these; SIGHUP is what closing the terminal (or
# `tmux kill-session`) sends. Their runs are left running (the next runner adopts them)
# unless --kill-runs.
STOP_SIGNALS = (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)


def _loopback(host: str) -> bool:
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return host == "localhost"


def ensure_home(quiet: bool = False) -> None:
    """A first run creates SLUICE_HOME with the default config.json."""
    path = default_home() / "config.json"
    if not path.exists():
        atomic_write_json(path, DEFAULT_CONFIG)
        if not quiet:
            print(f"sluice: wrote {path}", file=sys.stderr, flush=True)


def cmd_serve(a: argparse.Namespace, store: Store) -> int:
    import uvicorn

    from .mcp_server import build_server

    host = a.host or store.config["http"]["host"]
    port = a.port or int(store.config["http"]["port"])
    if not _loopback(host):
        print(f"sluice: WARNING: {host} is not loopback — the tools (fn_save, fn_call: running "
              "code) are served without authentication to anyone who can reach this port",
              file=sys.stderr, flush=True)
    runner = None if a.no_runner else Runner(store, kill_runs=a.kill_runs)
    if runner:  # else a separate `sluice loop` runs the steps and outlives server restarts
        store.listeners.append(runner.wake)
        thread = threading.Thread(target=runner.run_forever, name="sluice-runner", daemon=True)
        thread.start()
    stopping = threading.Event()  # ends the dashboard's streams, so shutdown need not wait

    class Server(uvicorn.Server):
        def handle_exit(self, sig, frame):
            stopping.set()
            super().handle_exit(sig, frame)

        @contextlib.contextmanager
        def capture_signals(self):
            # Shut down on STOP_SIGNALS without re-raising the signal afterwards, so the
            # runner stops its fns and the process exits 0.
            prev = {s: signal.signal(s, self.handle_exit) for s in STOP_SIGNALS}
            try:
                yield
            finally:
                for s, h in prev.items():
                    signal.signal(s, h)

    print(f"sluice: MCP at http://{host}:{port}/mcp, dashboard at http://{host}:{port}/",
          file=sys.stderr, flush=True)
    app = build_server(store, stopping).streamable_http_app(host=host)
    try:
        Server(uvicorn.Config(app, host=host, port=port, log_level="warning",
                              timeout_graceful_shutdown=5)).run()
    finally:
        stopping.set()
        if runner:
            runner.stop()
            thread.join(timeout=30)
    return 0


def cmd_loop(a: argparse.Namespace, store: Store) -> int:
    runner = Runner(store, kill_runs=a.kill_runs)
    for sig in STOP_SIGNALS:
        signal.signal(sig, lambda *_: runner.stop())
    runner.run_forever()
    return 0


def cmd_tool(a: argparse.Namespace, store: Store) -> int:
    """List the tools, or call one: prints its result (JSON, or text for docs/plan_view).
    Exits 1 on a tool error (printed to stderr) or a result with "ok": false (verify)."""
    import anyio

    from .mcp_server import build_server

    server = build_server(store)
    if a.name is None:
        for t in anyio.run(server.list_tools):
            first = (t.description or "").strip().splitlines()[0]
            print(f"{t.name:<16} {first}")
        return 0
    try:
        args = json.loads(a.args)
    except json.JSONDecodeError as e:
        print(json.dumps({"error": "bad_request", "message": f"args: not JSON: {e}"}),
              file=sys.stderr)
        return 1
    if not isinstance(args, dict):
        print(json.dumps({"error": "bad_request", "message": "args: expected a JSON object"}),
              file=sys.stderr)
        return 1
    res = anyio.run(server.call_tool, a.name, args)
    text = res.content[0].text if res.content else ""
    if res.is_error:
        print(text, file=sys.stderr)
        return 1
    if res.structured_content is None:
        print(text, end="" if text.endswith("\n") else "\n")
        return 0
    value: Any = json.loads(text)
    print(json.dumps(value, indent=2, ensure_ascii=False))
    return 1 if isinstance(value, dict) and value.get("ok") is False else 0


def cmd_watch(a: argparse.Namespace, store: Store) -> int:
    """Follow the project's (or the home's) log, one JSON line per matching record."""
    from . import log as L
    from .watch import follow

    if a.project is not None:
        store.project(a.project)
    kinds = [k for k in (a.kinds or "").split(",") if k]
    threads = [t for t in (a.threads or "").split(",") if t]
    errs = L.check_kinds(kinds)
    if errs:
        raise BadRequest("; ".join(errs))
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    with contextlib.suppress(KeyboardInterrupt, BrokenPipeError):
        follow(store.home, a.project, sys.stdout, kinds, threads, a.since_seq, wake=a.wake)
    return 0


def cmd_next(a: argparse.Namespace, store: Store) -> int:
    """Print the next record an orchestrator acts on once one arrives, then exit."""
    from .watch import line, next_up

    if a.project:
        projects = [store.project(p)["name"] for p in a.project]
    else:
        projects = [p["name"] for p in store.projects() if not p["archived"]]
        if not projects:
            raise BadRequest("no projects (none not archived); pass -p to name them")
    if a.cursor is not None:
        try:
            since = int(Path(a.cursor).read_text().strip())
        except FileNotFoundError:
            since = None  # a missing cursor starts from now
        except ValueError:
            raise BadRequest(f"cursor file {a.cursor} is not a seq") from None
    else:
        since = a.since_seq
    res = next_up(store, projects, since, me=a.me, timeout=a.timeout, every=a.all)
    if a.cursor is not None:
        atomic_write_text(Path(a.cursor), f"{res['last_seq']}\n")
    shown = [*res["notes"], *res["records"]]
    if a.json:
        for r in shown:
            print(json.dumps(r, ensure_ascii=False))
        print(json.dumps({"seq": res["last_seq"], "timed_out": res["timed_out"]}))
    else:
        for r in shown:
            print(line(r))
        print(f"{'timeout ' if res['timed_out'] else ''}seq {res['last_seq']}")
    sys.stdout.flush()
    return 0


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="sluice", description="Run typed plans of fns. Everything goes through the MCP "
        "tools: `sluice serve` exposes them, `sluice tool` calls them from the shell.")
    sub = p.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("serve", help="runner + MCP server + dashboard (with the inbox)")
    s.add_argument("--host")
    s.add_argument("--port", type=int)
    s.add_argument("--no-runner", action="store_true",
                   help="serve only; run `sluice loop` separately so restarting the server "
                   "leaves running steps alone")
    s.add_argument("--kill-runs", action="store_true",
                   help="on exit, stop the runs this runner started instead of leaving them "
                   "for the next runner to adopt")
    s = sub.add_parser("loop", help="runner only")
    s.add_argument("--kill-runs", action="store_true",
                   help="on exit, stop the runs this runner started instead of leaving them "
                   "for the next runner to adopt")
    s = sub.add_parser("tool", help="list the MCP tools, or call one in-process",
                       description="Without a name, list the tools. With one, call it with a "
                       "JSON object of arguments and print the result. Exit 1 on an error or "
                       'an "ok": false result.')
    s.add_argument("name", nargs="?")
    s.add_argument("args", nargs="?", default="{}", help="JSON object of arguments")
    s = sub.add_parser("watch", help="print new log records as JSON lines, until killed",
                       description="Follow a project's log (or, without -p, the home log) and "
                       "print each new matching record as one JSON line. Never exits; reads "
                       "the home's database only, so it needs no runner.")
    s.add_argument("-p", "--project")
    s.add_argument("--kinds", help="comma-separated kinds, e.g. step.status,message")
    s.add_argument("--threads", help="comma-separated thread names (messages on these only)")
    s.add_argument("--since-seq", type=int, help="start after this seq (default: from now)")
    s.add_argument("--wake", choices=("any", "questions"), default="any",
                   help="questions: hold notes (needs_reply false) and print them with the "
                   "next record that is not one")
    s = sub.add_parser("next", help="print the next record an orchestrator acts on, exit",
                       description="Block until a record one of the projects' logs should "
                       "wake an orchestrator for (a failed/stale/skipped step, an "
                       "open-fn or unit-completing success, a question for you, an inbox "
                       "post or answer), print it compactly and exit. Held notes print "
                       "just before it; the last line is `seq <N>` to pass to --since-seq.")
    s.add_argument("-p", "--project", action="append",
                   help="a project to watch (repeatable; default: every project not "
                   "archived)")
    where = s.add_mutually_exclusive_group()
    where.add_argument("--since-seq", type=int,
                       help="start after this seq (default: from now)")
    where.add_argument("--cursor", metavar="FILE",
                       help="read the start seq from FILE (missing: from now) and write "
                       "the last consumed seq back to it, atomically")
    s.add_argument("--me", default="orchestrator",
                   help="your name: your own messages never wake it, questions wake only "
                   "when addressed to this or to nobody (default: orchestrator)")
    s.add_argument("--timeout", type=float, default=None, metavar="S",
                   help="wait at most S seconds, then exit 0 printing `timeout seq <N>` "
                   "(default: wait forever)")
    s.add_argument("--all", action="store_true", help="every record wakes it")
    s.add_argument("--json", action="store_true",
                   help="print each record as one JSON line, then {\"seq\": N, ...}")
    return p


def main(argv: list[str] | None = None) -> int:
    a = build_parser().parse_args(argv)
    try:
        ensure_home(quiet=a.cmd in ("tool", "watch", "next"))
        store = Store()
        db.connect(store.home)  # refuses a home from before the SQLite store, up front
        return {"serve": cmd_serve, "loop": cmd_loop, "tool": cmd_tool,
                "watch": cmd_watch, "next": cmd_next}[a.cmd](a, store)
    except SluiceError as e:
        print(json.dumps(e.payload(), indent=2), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
