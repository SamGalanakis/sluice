"""A headless Chromium for the dashboard's browser tests, driven over the DevTools protocol on
a pipe (`--remote-debugging-pipe`: JSON messages ending in NUL on fds 3 and 4), so it needs
nothing but the standard library and a Chromium binary: `SLUICE_CHROME`, a Playwright download
in ~/.cache/ms-playwright, or chromium on PATH."""

from __future__ import annotations

import contextlib
import glob
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

TRAMPOLINE = ("import fcntl, os, sys\n"
              "high = [fcntl.fcntl(int(fd), fcntl.F_DUPFD, 10) for fd in sys.argv[1:3]]\n"
              "os.dup2(high[0], 3)\nos.dup2(high[1], 4)\nos.execv(sys.argv[3], sys.argv[3:])")


def find_chrome() -> str | None:
    if os.environ.get("SLUICE_CHROME"):
        return os.environ["SLUICE_CHROME"]
    cache = Path.home() / ".cache" / "ms-playwright"
    for pattern in ("chromium_headless_shell-*/chrome-headless-shell-linux64/chrome-headless-shell",
                    "chromium-*/chrome-linux64/chrome"):
        found = sorted(glob.glob(str(cache / pattern)))
        if found:
            return found[-1]
    return shutil.which("chromium") or shutil.which("chromium-browser") \
        or shutil.which("google-chrome")


class Chrome:
    def __init__(self, exe: str):
        to_chrome, self._w = os.pipe()
        self._r, from_chrome = os.pipe()
        self._profile = tempfile.mkdtemp(prefix="sluice-chrome-")
        args = [exe, "--headless", "--remote-debugging-pipe", "--no-sandbox", "--disable-gpu",
                "--no-first-run", f"--user-data-dir={self._profile}"]
        proxy = os.environ.get("HTTPS_PROXY") or os.environ.get("https_proxy")
        if proxy:
            args.append(f"--proxy-server={proxy}")
        # Chromium reads commands on fd 3 and writes replies on fd 4: a small Python
        # trampoline moves the pipes there (by way of high fds, so neither clobbers the other).
        self.proc = subprocess.Popen([sys.executable, "-c", TRAMPOLINE, str(to_chrome),
                                      str(from_chrome), *args],
                                     pass_fds=(to_chrome, from_chrome),
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                     start_new_session=True)
        os.close(to_chrome)
        os.close(from_chrome)
        self._buf = b""
        self._id = 0
        self.session: str | None = None

    def send(self, method: str, params: dict | None = None, timeout: float = 30) -> dict:
        self._id += 1
        msg: dict[str, Any] = {"id": self._id, "method": method, "params": params or {}}
        if self.session and not method.startswith("Target."):
            msg["sessionId"] = self.session
        os.write(self._w, json.dumps(msg).encode() + b"\0")
        deadline = time.time() + timeout
        while time.time() < deadline:
            while b"\0" in self._buf:
                raw, self._buf = self._buf.split(b"\0", 1)
                reply = json.loads(raw)
                if reply.get("id") == self._id:
                    if "error" in reply:
                        raise RuntimeError(f"{method}: {reply['error']}")
                    return reply["result"]
            chunk = os.read(self._r, 1 << 16)
            if not chunk:
                raise RuntimeError("chromium exited")
            self._buf += chunk
        raise TimeoutError(method)

    def open(self, url: str) -> None:
        target = self.send("Target.createTarget", {"url": "about:blank"})["targetId"]
        self.session = self.send("Target.attachToTarget",
                                 {"targetId": target, "flatten": True})["sessionId"]
        self.send("Page.navigate", {"url": url})

    def eval(self, expression: str) -> Any:
        res = self.send("Runtime.evaluate", {"expression": expression, "awaitPromise": True,
                                             "returnByValue": True})
        if "exceptionDetails" in res:
            raise RuntimeError(res["exceptionDetails"])
        return res["result"].get("value")

    def wait(self, expression: str, timeout: float = 20) -> Any:
        """Evaluate until the value is truthy; returns it."""
        deadline = time.time() + timeout
        while True:
            value = self.eval(expression)
            if value:
                return value
            if time.time() > deadline:
                raise TimeoutError(expression)
            time.sleep(0.1)

    def close(self) -> None:
        """Kill Chromium and every process it started (its own process group)."""
        with contextlib.suppress(ProcessLookupError):
            os.killpg(self.proc.pid, signal.SIGKILL)
        self.proc.wait()
        os.close(self._w)
        os.close(self._r)
        shutil.rmtree(self._profile, ignore_errors=True)
