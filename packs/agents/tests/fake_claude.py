"""A scripted stand-in for interactive Claude Code, run in the supervisor's tmux pane.

It speaks the channels the Claude adapter reads: the composer (fake_composer.Composer), the
hooks of the `--settings` file (each run with its JSON payload on stdin), the status file
`$CLAUDE_CONFIG_DIR/sessions/<pid>.json`, the transcript under
`$CLAUDE_CONFIG_DIR/projects/<cwd slug>/<session>.jsonl`, and `/exit`, which records
`lastCost` in `$CLAUDE_CONFIG_DIR/.claude.json` as Claude Code does (under the main worktree's
root inside a git repo).

`$FAKE_CLAUDE` names a JSON config: `argv` and `prompts` (files to record its argv and every
message it receives), `trust` (show the workspace-trust dialog first), `cost`, `exit_at_start`
(print `stderr` and exit with that code), and `turns`, played one per message (or background
notification). A turn may hold: `busy_s`, `tool` ({name, input}), `tool_error`, `run` (a shell
command in cwd), `submit` (outputs stored as the run's submission, as step_submit does), `submit_cli` (a value
put in every placeholder of the task's step_submit command, run through the sluice CLI),
`reply`, `error` (the turn ends with StopFailure), `exit` (print `stderr`, exit with that code),
`background_s` (a background shell for that long, then a task notification), `wakeup_s` (a
ScheduleWakeup that fires after that long). Past the last turn it replies "ok". The turns
played so far are counted in `$FAKE_CLAUDE.cursor`, so a resumed session carries on."""

import json
import os
import re
import subprocess
import sys
import time
import uuid
from collections import deque
from pathlib import Path

from fake_composer import Composer, Raw, draw

CFG = json.loads(Path(os.environ["FAKE_CLAUDE"]).read_text())
CURSOR = Path(os.environ["FAKE_CLAUDE"] + ".cursor")  # turns played, across processes
CONFIG = Path(os.environ["CLAUDE_CONFIG_DIR"])



def submit(outputs):
    """What step_submit stores for the run: its submission, which the supervisor reads."""
    from sluice import db

    with db.write(os.environ["SLUICE_HOME"]) as conn:
        conn.execute("INSERT OR REPLACE INTO submissions (project, run, step, outputs, at) "
                     "VALUES (?, ?, ?, ?, 'now')",
                     (os.environ["SLUICE_PROJECT"], os.environ["SLUICE_RUN_ID"],
                      os.environ["SLUICE_STEP"], json.dumps(outputs)))

def arg(argv, name):
    return argv[argv.index(name) + 1] if name in argv else None


class Fake:
    def __init__(self, argv):
        self.cwd = os.getcwd()
        self.settings = json.loads(Path(arg(argv, "--settings")).read_text())
        resume = arg(argv, "--resume")
        self.sid = resume or str(uuid.uuid4())
        slug = re.sub(r"[^A-Za-z0-9]", "-", self.cwd)
        self.transcript = CONFIG / "projects" / slug / f"{self.sid}.jsonl"
        if resume and not self.transcript.exists():
            print(f"No conversation found with session ID: {resume}", file=sys.stderr)
            sys.exit(1)
        self.transcript.parent.mkdir(parents=True, exist_ok=True)
        self.status_file = CONFIG / "sessions" / f"{os.getpid()}.json"
        self.status_file.parent.mkdir(parents=True, exist_ok=True)
        played = int(CURSOR.read_text()) if CURSOR.exists() else 0
        self.turns = deque(CFG.get("turns", [])[played:])
        self.queue = deque()
        self.busy_until = None
        self.turn = None
        self.timers = []
        self.background = []
        self.crons = []
        self.composer = Composer()
        self.set_status("idle")
        self.entry({"type": "permission-mode", "cwd": self.cwd, "sessionId": self.sid})
        self.hook("SessionStart", source="resume" if resume else "startup")

    def set_status(self, status):
        self.status = status
        tmp = self.status_file.with_suffix(".tmp")
        tmp.write_text(json.dumps({"pid": os.getpid(), "sessionId": self.sid, "cwd": self.cwd,
                                   "kind": "interactive", "status": status}))
        tmp.replace(self.status_file)

    def entry(self, rec):
        with open(self.transcript, "a") as f:
            f.write(json.dumps({"sessionId": self.sid, "cwd": self.cwd, **rec}) + "\n")

    def hook(self, event, **payload):
        payload = {"session_id": self.sid, "transcript_path": str(self.transcript),
                   "cwd": self.cwd, "hook_event_name": event, **payload}
        for group in self.settings.get("hooks", {}).get(event, []):
            for h in group.get("hooks", []):
                subprocess.run(["sh", "-c", h["command"]], input=json.dumps(payload),
                               text=True, check=False)

    def assistant(self, *content, **extra):
        self.entry({"type": "assistant", "message": {"role": "assistant",
                                                     "content": list(content)}, **extra})

    # turns
    def start(self, kind, text):
        self.turn = self.turns.popleft() if self.turns else {}
        CURSOR.write_text(str(int(CURSOR.read_text()) + 1 if CURSOR.exists() else 1))
        self.set_status("busy")
        self.hook("UserPromptSubmit", prompt=text)
        user = {"type": "user", "message": {"role": "user", "content": text}}
        if kind == "task-notification":
            user["origin"] = {"kind": "task-notification"}
        elif kind == "wakeup":
            self.entry({"type": "system", "subtype": "scheduled_task_fire",
                        "content": "Claude resuming /loop wakeup"})
            user["isMeta"] = True
        self.entry(user)
        self.task = text
        self.busy_until = time.monotonic() + float(self.turn.get("busy_s", 0.2))

    def finish(self):
        turn, self.busy_until = self.turn, None
        if "exit" in turn:
            draw([turn.get("stderr", "")])
            sys.exit(turn["exit"])
        if turn.get("tool"):
            self.assistant({"type": "tool_use", "id": "t1", "name": turn["tool"]["name"],
                            "input": turn["tool"].get("input", {})})
        if turn.get("tool_error"):
            self.entry({"type": "user", "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "is_error": True,
                 "content": turn["tool_error"]}]}})
        if turn.get("run"):
            subprocess.run(turn["run"], shell=True, cwd=self.cwd, check=False,
                           capture_output=True)
        if "submit" in turn:
            submit(turn["submit"])
        if "submit_cli" in turn:
            self.submit_cli(turn["submit_cli"])
        if turn.get("background_s"):
            self.start_background(float(turn["background_s"]))
        if turn.get("wakeup_s"):
            at = time.time() + float(turn["wakeup_s"])
            self.assistant({"type": "tool_use", "id": "w1", "name": "ScheduleWakeup",
                            "input": {"delaySeconds": turn["wakeup_s"]}})
            self.entry({"type": "user", "toolUseResult": {"scheduledFor": int(at * 1000)},
                        "message": {"role": "user", "content": [
                            {"type": "tool_result", "tool_use_id": "w1", "content": "ok"}]}})
            self.crons.append({"id": "c1", "schedule": "* * * * *", "recurring": False})
            self.timers.append((time.monotonic() + float(turn["wakeup_s"]), self.wake))
        reply = turn.get("reply", "ok")
        if turn.get("error"):
            self.assistant({"type": "text", "text": turn["error"]}, isApiErrorMessage=True)
            self.hook("StopFailure", error=turn["error"], last_assistant_message=turn["error"])
        else:
            self.assistant({"type": "text", "text": reply})
            self.hook("Stop", last_assistant_message=reply, background_tasks=[
                {"id": "b1", "type": "shell", "status": "running", "description": "sleep"}
                for _ in self.background], session_crons=list(self.crons))
        self.set_status("shell" if self.background else "idle")

    def submit_cli(self, value):
        task = self.task
        pointer = re.search(r"Your task is in (\S+); read it", task)
        if pointer:
            task = Path(pointer.group(1)).read_text()
        cmd = re.search(r"sluice tool step_submit '(.*?)'`", task).group(1)
        args = re.sub(r"<[^>]+>", json.dumps(value), cmd)
        subprocess.run([sys.executable, "-m", "sluice.cli", "tool", "step_submit", args],
                       check=False, capture_output=True)

    def start_background(self, seconds):
        proc = subprocess.Popen(["sleep", str(seconds + 30)])  # outlives its notification
        self.background.append(proc)
        self.timers.append((time.monotonic() + seconds, lambda: self.background_done(proc)))

    def background_done(self, proc):
        proc.kill()
        proc.wait()
        self.background.remove(proc)
        note = "<task-notification><summary>sleep finished</summary></task-notification>"
        self.queue.append(("task-notification", note))

    def wake(self):
        self.crons = []
        self.queue.append(("wakeup", "Reply with the single word AWAKE."))

    def exit(self):
        cfg_file = CONFIG / ".claude.json"
        cfg = json.loads(cfg_file.read_text()) if cfg_file.exists() else {}
        common = subprocess.run(["git", "rev-parse", "--path-format=absolute",
                                 "--git-common-dir"], cwd=self.cwd, capture_output=True,
                                text=True, check=False).stdout.strip()
        key = str(Path(common).parent) if common.endswith("/.git") else self.cwd
        cfg.setdefault("projects", {})[key] = {"lastCost": CFG.get("cost", 0.02),
                                               "lastSessionId": self.sid}
        cfg_file.write_text(json.dumps(cfg))
        self.status_file.unlink(missing_ok=True)
        sys.exit(0)

    def tick(self, data):
        for msg in self.composer.feed(data):
            with open(CFG["prompts"], "a") as f:
                f.write(json.dumps(msg) + "\n")
            if msg.strip() == "/exit":
                self.exit()
            self.queue.append(("human", msg))
        now = time.monotonic()
        for t, fn in [x for x in self.timers if x[0] <= now]:
            self.timers.remove((t, fn))
            fn()
        if self.busy_until is not None and now >= self.busy_until:
            self.finish()
        if self.busy_until is None and self.queue:
            self.start(*self.queue.popleft())
        above = ["✻ Working…"] if self.status == "busy" else []
        draw(above + self.composer.lines())


def trust(term):
    """The workspace-trust dialog; its default answer is No."""
    choice = 0
    while True:
        rows = ["No, exit", "Yes, I trust this folder"]
        draw(["Accessing workspace:", "Quick safety check: Is this a project you trust?"]
             + [("❯ " if i == choice else "  ") + r for i, r in enumerate(rows)]
             + ["Enter to confirm · Esc to cancel"])
        data = term.read(0.2)
        if b"\x1b[B" in data:
            choice = 1
        if b"\r" in data:
            if choice == 0:
                sys.exit(1)
            return


def main():
    argv = sys.argv[1:]
    with open(CFG["argv"], "w") as f:
        json.dump(argv, f)
    if CFG.get("exit_at_start") is not None:
        print(CFG.get("stderr", ""), flush=True)
        time.sleep(0.5)
        sys.exit(CFG["exit_at_start"])
    with Raw() as term:
        if CFG.get("trust"):
            trust(term)
        fake = Fake(argv)
        while True:
            fake.tick(term.read(0.05))


if __name__ == "__main__":
    main()
