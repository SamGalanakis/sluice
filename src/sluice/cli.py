"""The `sluice` command line (SPEC §9): `serve`, `loop`, and `tool` to call any MCP tool
in-process through the same server object `serve` exposes."""

from __future__ import annotations

import argparse
import contextlib
import json
import signal
import sys
import threading
from typing import Any

from .errors import SluiceError
from .runner import Runner
from .store import DEFAULT_CONFIG, Store, default_home
from .util import atomic_write_json


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

    print(f"sluice: MCP at http://{host}:{port}/mcp, dashboard at http://{host}:{port}/",
          file=sys.stderr, flush=True)
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


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="sluice", description="Run typed plans of fns. Everything goes through the MCP "
        "tools: `sluice serve` exposes them, `sluice tool` calls them from the shell.")
    sub = p.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("serve", help="runner + MCP server + read-only dashboard")
    s.add_argument("--host")
    s.add_argument("--port", type=int)
    sub.add_parser("loop", help="runner only")
    s = sub.add_parser("tool", help="list the MCP tools, or call one in-process",
                       description="Without a name, list the tools. With one, call it with a "
                       "JSON object of arguments and print the result. Exit 1 on an error or "
                       'an "ok": false result.')
    s.add_argument("name", nargs="?")
    s.add_argument("args", nargs="?", default="{}", help="JSON object of arguments")
    return p


def main(argv: list[str] | None = None) -> int:
    a = build_parser().parse_args(argv)
    try:
        ensure_home(quiet=a.cmd == "tool")  # tool output stays pure JSON
        store = Store()
        return {"serve": cmd_serve, "loop": cmd_loop, "tool": cmd_tool}[a.cmd](a, store)
    except SluiceError as e:
        print(json.dumps(e.payload(), indent=2), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
