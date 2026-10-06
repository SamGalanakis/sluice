"""Protocol 1 custom function helper. Standard library only.

Callbacks use the pinned run capability; they never open the home database.
"""
from __future__ import annotations

import contextlib
import json
import math
import os
import signal
import socket
import struct
import subprocess
import sys
import threading
import time
import traceback
import uuid
from pathlib import Path

MAX_BYTES = 16 * 1024 * 1024
TAIL = 2048


class Transient(Exception):
    """Retry main inside this same run, within its explicit retry budget."""


class AgentFailure(Exception):
    """Terminal native-agent failure returned by ctx.builtin."""
    def __init__(self, kind, message, session=None):
        self.kind, self.message, self.session = kind, message, session
        super().__init__(message)


class Rejected(Exception):
    """Intentional project refusal, eligible for a registered completion action."""


class Cancelled(Exception):
    pass


class CallbackError(Exception):
    def __init__(self, error):
        self.envelope = error
        self.error = error.get("error")
        self.message = error.get("message", str(error))
        self.errors = error.get("errors", [])
        self.current_rev = error.get("current_rev")
        self.retryable = error.get("retryable", False)
        super().__init__(self.message)


class ShError(Exception):
    def __init__(self, argv, code, stdout, stderr):
        self.argv, self.code, self.stdout, self.stderr = argv, code, stdout, stderr
        super().__init__(f"{argv[0]} exited {code}: {_tail(stderr.strip())}")


def _tail(value):
    return str(value).encode("utf-8", errors="replace")[-TAIL:].decode("utf-8", errors="ignore")


def _pairs(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def _integer(value):
    number = int(value)
    if not -(2**63) <= number < 2**63:
        raise ValueError("JSON integer outside i64")
    return number


def _float(value):
    number = float(value)
    if not math.isfinite(number):
        raise ValueError("non-finite JSON number")
    return number


def _loads(raw):
    if len(raw) > MAX_BYTES:
        raise ValueError("JSON exceeds 16 MiB")
    return json.loads(raw.decode("utf-8"), object_pairs_hook=_pairs, parse_int=_integer,
                      parse_float=_float, parse_constant=lambda x: _float(x))


def _dumps(value):
    raw = json.dumps(value, allow_nan=False, ensure_ascii=False).encode("utf-8")
    _loads(raw)
    return raw


def log(message):
    print(message, file=sys.stderr, flush=True)


class Context:
    def __init__(self, data):
        self.project_id = data["project_id"]
        self.project = data.get("project", "")
        self.step = data.get("step", "")
        self.run_id = data["run_id"]
        self.attempt_id = data["attempt_id"]
        self.invocation_id = data["invocation_id"]
        self.run_dir = Path(data["run_dir"])
        self.home = Path(data["home"])
        self.fn_dir = Path(data["fn_dir"])
        self.project_dir = Path(data["project_dir"])
        self.prev_run = data.get("prev_run")
        self.extra_inputs = data.get("extra_inputs", {})
        self.outputs = data.get("outputs", {})
        self.attempt = 1
        self._endpoint = data.get("control_socket")
        self._capability = data.get("run_capability")
        self._cancel = threading.Event()
        self._action = None

    log = staticmethod(log)

    def _wait(self, seconds):
        if self._cancel.wait(seconds):
            raise Cancelled("run cancelled")

    def callback(self, command, args=None):
        """Return a typed CommandReply object, or raise CallbackError."""
        if self._cancel.is_set():
            raise Cancelled("run cancelled")
        request_id = uuid.uuid4().hex
        request = {"protocol": 1, "request_id": request_id,
                   "run_capability": self._capability,
                   "command": {"command": command}}
        if args is not None:
            request["command"]["args"] = args
        raw = _dumps(request)
        if self._endpoint:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as conn:
                conn.connect(self._endpoint)
                conn.sendall(struct.pack(">I", len(raw)) + raw)

                def read_exact(count):
                    chunks = bytearray()
                    while len(chunks) < count:
                        chunk = conn.recv(min(65536, count - len(chunks)))
                        if not chunk:
                            raise ValueError("incomplete callback frame")
                        chunks.extend(chunk)
                    return bytes(chunks)

                size = struct.unpack(">I", read_exact(4))[0]
                if size > MAX_BYTES:
                    raise ValueError("callback frame exceeds 16 MiB")
                reply = _loads(read_exact(size))
        else:
            raise RuntimeError("run has no control socket")
        if type(reply.get("protocol")) is not int or reply["protocol"] != 1 or reply.get("request_id") != request_id:
            raise ValueError("callback reply identity mismatch")
        result = reply["result"]
        if result["status"] == "error":
            error = result["value"]
            if error.get("error") == "transient":
                raise Transient(error["message"])
            if error.get("error") == "agent_failure":
                raise AgentFailure(error.get("kind", "AgentFailure"), error["message"],
                                   error.get("session"))
            if error.get("error") == "cancelled":
                raise Cancelled(error["message"])
            raise CallbackError(error)
        if result["status"] != "ok":
            raise ValueError("invalid callback result status")
        return result["value"]

    def _data(self, command, args):
        reply = self.callback(command, args)
        if reply.get("reply") != "data":
            raise ValueError(f"{command} did not return data")
        return reply["data"]

    def tool(self, name: str, args: dict):
        """Call a tool with the flat arguments MCP and `sluice tool` take; return its result.

        `project` defaults to the run's own. The result is what MCP returns: the tool's JSON,
        `{"ok": True}` for an acknowledgement.
        """
        if not isinstance(name, str) or not name or not isinstance(args, dict):
            raise ValueError("tool requires a name and an argument object")
        args = dict(args)
        args.setdefault("project", os.environ.get("SLUICE_PROJECT_ID", self.project_id))
        return self._data("tool", {"name": name, "args": args})

    def builtin(self, name, request):
        return self._data("builtin", {"invocation": {
            "project": self.project_id, "step": self.step or None, "run": self.run_id,
            "attempt": self.attempt_id, "invocation": _uuid7(),
            "name": name, "inputs": request}})

    def submission(self):
        return self._data("submission", {"run": self.run_id})

    def submit(self, outputs):
        """Submit the step's declared outputs, once: a valid submission is the run's
        done signal (it ends an agent's session), and a second one is refused."""
        return self.callback("step_submit", {"project": self.project_id, "step": self.step,
                                             "run": self.run_id, "outputs": outputs,
                                             "author": self.step})

    def progress(self, outputs=None, /, **fields):
        """Publish the step's latest values while it runs, without finishing it:
        `ctx.progress(red=3)` or `ctx.progress({"red": 3})`. Each field is one of the step's
        outputs and must fit its type; fields merge over this run's earlier progress. Never
        final: nothing reads it as an output, and it wakes no wait. Returns {project, step,
        run, progress, at}."""
        values = dict(outputs or {})
        values.update(fields)
        if not values:
            raise ValueError("progress needs at least one field")
        return self.tool("step_progress", {"step": self.step, "run": self.run_id,
                                           "outputs": values})

    def retry_on_failure(self, step, message):
        """The receiver captures and registers the store target in one transaction."""
        if not isinstance(step, str) or not step or not isinstance(message, str) or not message:
            raise ValueError("retry target and message must be nonempty strings")
        if len(message.encode()) > 8192:
            raise ValueError("retry message exceeds 8 KiB")
        action = (step, message)
        if self._action is not None:
            if self._action != action:
                raise ValueError("completion action already registered")
            return
        reply = self.callback("retry_on_failure", {"project": self.project_id,
            "run": self.run_id, "step": step, "message": message, "author": self.step})
        if reply.get("reply") != "ack":
            raise ValueError("completion registration did not acknowledge")
        self._action = action

    @contextlib.contextmanager
    def acquire(self, resource, amount=1, timeout=None):
        if not isinstance(amount, int) or isinstance(amount, bool) or amount <= 0:
            raise ValueError("lease amount must be a positive integer")
        if timeout is not None and (not math.isfinite(timeout) or timeout < 0):
            raise ValueError("lease timeout must be finite and nonnegative")
        request = {"run": self.run_id, "resource": resource, "amount": amount,
                   "priority": 0, "request_id": uuid.uuid4().hex}
        deadline = None if timeout is None else time.monotonic() + timeout
        lease = None
        try:
            while True:
                reply = self.callback("acquire_lease", request)
                if reply.get("reply") != "lease":
                    raise ValueError("acquire did not return a lease")
                data = reply["data"]
                if lease is not None and lease != data["lease"]:
                    raise ValueError("lease identity changed while waiting")
                lease = data["lease"]
                if data["state"] == "held":
                    break
                if data["state"] != "waiting":
                    raise ValueError("lease released before grant")
                remaining = None if deadline is None else deadline - time.monotonic()
                if remaining is not None and remaining <= 0:
                    raise TimeoutError("section lease timed out")
                self._wait(0.05 if remaining is None else min(0.05, remaining))
            yield
        finally:
            if lease is not None:
                # Cancellation still permits voluntary release of an acquired/waiting lease.
                cancelled = self._cancel.is_set()
                self._cancel.clear()
                try:
                    self.callback("release_lease", {"lease": lease, "run": self.run_id})
                finally:
                    if cancelled:
                        self._cancel.set()

    def header(self, text):
        """Add the outer step's declared submission fields to an agent task."""
        if not self.outputs:
            return text
        schema = json.dumps(self.outputs, ensure_ascii=False)
        return (f"{text}\n\nDeclared outputs for run {self.run_id}: {schema}\nSubmit through the run "
                "callback. Submit only when you are finished: submitting ends your session.")


def _uuid7():
    # RFC 9562, millisecond timestamp with 74 random bits. No external uuid library.
    random = uuid.uuid4().int
    return str(uuid.UUID(int=(int(time.time() * 1000) << 80) | (7 << 76)
                         | (random & ((1 << 76) - 1) & ~(3 << 62)) | (2 << 62)))


def run(main, retries=0, backoff=30.0):
    """Read one envelope; call main N+1 times at most; write exactly one result."""
    real_stdout = sys.stdout
    ctx = None
    previous = {}
    code = 1
    try:
        if not isinstance(retries, int) or isinstance(retries, bool) or retries < 0:
            raise ValueError("retries must be a nonnegative integer")
        backoff = float(os.environ.get("SLUICE_BACKOFF", backoff))
        if not math.isfinite(backoff) or backoff < 0:
            raise ValueError("backoff must be finite and nonnegative")
        envelope = _loads(sys.stdin.buffer.read(MAX_BYTES + 1))
        if (set(envelope) != {"protocol", "inputs", "context"}
                or type(envelope["protocol"]) is not int or envelope["protocol"] != 1):
            raise ValueError("invalid protocol 1 input envelope")
        if not isinstance(envelope["inputs"], dict) or not isinstance(envelope["context"], dict):
            raise ValueError("inputs and context must be objects")
        ctx = Context(envelope["context"])

        def cancel(*_):
            ctx._cancel.set()
            raise Cancelled("run cancelled")

        for signum in (signal.SIGTERM, signal.SIGINT):
            previous[signum] = signal.signal(signum, cancel)
        with contextlib.redirect_stdout(sys.stderr):
            while True:
                if ctx._cancel.is_set():
                    raise Cancelled("run cancelled")
                try:
                    out = main(envelope["inputs"], ctx)
                    break
                except Transient as error:
                    if ctx.attempt > retries:
                        raise
                    log(f"transient (attempt {ctx.attempt}): {error}; retrying in {backoff}s")
                    ctx._wait(backoff)
                    ctx.attempt += 1
            if out is None:
                out = {}
            if not isinstance(out, dict):
                raise TypeError("main must return a dict")
            answer = _dumps({"ok": True, "outputs": out})
            code = 0
    except BaseException as error:
        traceback.print_exc(file=sys.stderr)
        kind = ("rejected" if isinstance(error, Rejected) else
                "transient" if isinstance(error, Transient) else
                "cancelled" if isinstance(error, (Cancelled, KeyboardInterrupt)) else "fn_failure")
        detail = ({"error": "agent_failure", "kind": error.kind, "message": _tail(error.message),
                   "session": error.session} if isinstance(error, AgentFailure) else
                  {"kind": kind, "message": _tail(error)})
        answer = _dumps({"ok": False, "error": detail})
    finally:
        for signum, handler in previous.items():
            signal.signal(signum, handler)
    real_stdout.buffer.write(answer)
    real_stdout.flush()
    raise SystemExit(code)


def child_env(extra=None):
    """Restore host tools, strip uv/Python and agent nesting contamination."""
    env = dict(os.environ)
    host_path = env.get("SLUICE_HOST_PATH")
    if host_path is not None:
        env["PATH"] = host_path
    elif env.get("VIRTUAL_ENV"):
        venv_bin = str(Path(env["VIRTUAL_ENV"]) / "bin")
        env["PATH"] = os.pathsep.join(p for p in env.get("PATH", "").split(os.pathsep)
                                      if p.rstrip("/") != venv_bin.rstrip("/"))
    for name in ("PYTHONPATH", "VIRTUAL_ENV"):
        original = env.get("SLUICE_HOST_" + name, "")
        if original:
            env[name] = original
        else:
            env.pop(name, None)
    for name in ("PYTHONHOME", "UV_PROJECT_ENVIRONMENT", "UV_ACTIVE", "CONDA_PREFIX",
                 "CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT", "CODEX_THREAD_ID"):
        env.pop(name, None)
    env.update(extra or {})
    return env


def sh(argv, cwd=None, check=True, env=None, timeout=None, input=None):
    log(f"$ {' '.join(map(str, argv))}")
    result = subprocess.run(argv, cwd=cwd, env=child_env(env), timeout=timeout,
                            input=input, text=True, capture_output=True, check=False)
    for output in (result.stdout, result.stderr):
        if output.strip():
            log(_tail(output).rstrip())
    if check and result.returncode:
        raise ShError(argv, result.returncode, result.stdout, result.stderr)
    return result


def _feed(pipe, data):
    try:
        pipe.write(data)
    except BrokenPipeError:
        pass
    finally:
        pipe.close()


def stream(argv, on_line=None, cwd=None, check=True, env=None, follow=None, input=None):
    """Stream both pipes and an optional appended log; retain command output."""
    log(f"$ {' '.join(map(str, argv))}")
    on_line = on_line or (lambda line, source: log(" ".join(line.split())[:200]))
    output = {"stdout": [], "stderr": []}
    lock = threading.Lock()
    done = threading.Event()
    failures = []

    def emit(line, source):
        with lock:
            try:
                on_line(line.rstrip("\r\n"), source)
            except BaseException as error:
                failures.append(error)

    def pump(pipe, source):
        with pipe:
            for line in pipe:
                output[source].append(line)
                emit(line, source)

    def watch(path, position, identity):
        pending = b""
        while True:
            finished = done.is_set()
            try:
                stat = path.stat()
                current = (stat.st_dev, stat.st_ino)
                if (identity is not None and current != identity) or stat.st_size < position:
                    position, pending = 0, b""
                identity = current
                with path.open("rb") as file:
                    file.seek(position)
                    data = file.read()
                position += len(data)
                *lines, pending = (pending + data).split(b"\n")
                for line in lines:
                    emit(line.decode(errors="replace"), "follow")
            except OSError:
                pass
            if finished:
                if pending:
                    emit(pending.decode(errors="replace"), "follow")
                return
            done.wait(0.05)

    followed = Path(follow) if follow is not None else None
    initial = followed.stat() if followed and followed.exists() else None
    start = initial.st_size if initial else 0
    identity = (initial.st_dev, initial.st_ino) if initial else None
    process = subprocess.Popen(argv, cwd=cwd, env=child_env(env),
                               stdin=subprocess.PIPE if input is not None else subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               text=True, errors="replace", bufsize=1)
    workers = [threading.Thread(target=pump, args=(process.stdout, "stdout")),
               threading.Thread(target=pump, args=(process.stderr, "stderr"))]
    if input is not None:
        workers.append(threading.Thread(target=_feed, args=(process.stdin, input)))
    if followed:
        workers.append(threading.Thread(target=watch, args=(followed, start, identity)))
    for worker in workers:
        worker.start()
    try:
        code = process.wait()
    except BaseException:
        process.kill()
        process.wait()
        raise
    finally:
        done.set()
        for worker in workers:
            worker.join()
    if failures:
        raise failures[0]
    result = subprocess.CompletedProcess(argv, code, "".join(output["stdout"]), "".join(output["stderr"]))
    if check and code:
        raise ShError(argv, code, result.stdout, result.stderr)
    return result
