"""Scripted Devin TUI for the native adapter tests."""

import json
import os
import re
import subprocess
import sys
import time
import uuid
from pathlib import Path

from fake_composer import RULE, Composer, Raw, draw

CFG = json.loads(Path(os.environ["FAKE_DEVIN"]).read_text())



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


def hook(config, event, sid, **data):
    payload = {"hook_event_name": event, "session_id": sid, **data}
    for group in config.get("hooks", {}).get(event, []):
        for item in group.get("hooks", []):
            subprocess.run(["sh", "-c", item["command"]], input=json.dumps(payload),
                           text=True, check=False)


def task_text(message):
    found = re.search(r"Your task is in (\S+); read it", message)
    return Path(found.group(1)).read_text() if found else message


def main():
    argv = sys.argv[1:]
    Path(CFG["argv"]).write_text(json.dumps(argv))
    config = json.loads(Path(arg(argv, "--config")).read_text())
    sid = arg(argv, "--resume") or str(uuid.uuid4())
    cursor = Path(CFG["cursor"])
    turns = CFG.get("turns", [])
    composer = Composer()
    hook(config, "SessionStart", sid)
    with Raw() as term:
        while True:
            for message in composer.feed(term.read(0.05)):
                if message.strip() == "/exit":
                    hook(config, "SessionEnd", sid)
                    sys.exit(0)
                with Path(CFG["prompts"]).open("a") as f:
                    f.write(json.dumps(message) + "\n")
                index = int(cursor.read_text()) if cursor.exists() else 0
                cursor.write_text(str(index + 1))
                turn = turns[index] if index < len(turns) else {"reply": "ok"}
                hook(config, "UserPromptSubmit", sid, prompt=message, prompt_id=str(index))
                if turn.get("tool"):
                    hook(config, "PreToolUse", sid, tool_name=turn["tool"],
                         tool_input={"command": "echo done"}, prompt_id=str(index))
                if turn.get("busy_s"):
                    time.sleep(turn["busy_s"])
                if "submit" in turn:
                    submit(turn["submit"])
                if "submit_cli" in turn:
                    cmd = re.search(r"sluice tool step_submit '(.*?)'`",
                                    task_text(message)).group(1)
                    args = re.sub(r"<[^>]+>", json.dumps(turn["submit_cli"]), cmd)
                    subprocess.run([sys.executable, "-m", "sluice.cli", "tool", "step_submit",
                                    args], capture_output=True, check=False)
                if turn.get("exit"):
                    draw([turn.get("stderr", "")])
                    sys.exit(turn["exit"])
                hook(config, "Stop", sid, last_assistant_message=turn.get("reply", "ok"),
                     prompt_id=str(index), **({"error": turn["error"]} if "error" in turn else {}))
                Path(arg(argv, "--export")).write_text(json.dumps({
                    "session_id": sid, "steps": [{"source": "agent",
                                                   "message": turn.get("reply", "ok")}] }))
            draft = composer.draft.split("\n")[0]
            placeholder = "Ask Devin to build features, fix bugs, or work on your code"
            draw([RULE, "❯ " + (draft or placeholder), RULE,
                  arg(argv, "--model") or "swe-2-high"])


if __name__ == "__main__":
    main()
