"""A tiny stand-in for Claude Code's input box, for tests that drive a real tmux pane.

It turns on bracketed paste, draws the box the way Claude Code 2.1.283 does (a rule, `❯ ` and
the draft, a large paste collapsed to `[Pasted text #N +M lines]` on the row below, a rule),
and treats input like the real one: text inside paste markers is data (CR is a newline), an
Enter outside a paste submits, unless the draft ends in a backslash (a line continuation).
`drop_enters` folds that many Enters into the draft instead, as an Enter landing mid-paste is.
With `wrap`, a draft wider than that is word-wrapped on the rows under the glyph, as Claude
Code does in a narrow box.

Run as a script, `fake_composer.py OUT [BOOT_S] [DROP_ENTERS] [WRAP]` records each submitted
message as a JSON line in OUT."""

import json
import os
import select
import sys
import termios
import textwrap
import time
import tty

RULE = "─" * 100
PASTE_START, PASTE_END = b"\x1b[200~", b"\x1b[201~"


class Composer:
    def __init__(self, drop_enters=0, wrap=0):
        self.wrap = wrap
        self.draft = ""
        self.pastes = 0
        self.collapsed = False
        self.buf = b""
        self.in_paste = False
        self.drop_enters = drop_enters

    def feed(self, data):
        """Take input bytes; return the messages submitted by them."""
        self.buf += data
        sent = []
        while self.buf:
            if self.in_paste:
                end = self.buf.find(PASTE_END)
                if end < 0:
                    return sent  # wait for the rest of the paste
                text = self.buf[:end].decode("utf-8", "replace").replace("\r", "\n")
                self.buf = self.buf[end + len(PASTE_END):]
                self.in_paste = False
                self.draft += text
                if len(text) > 800 or text.count("\n") > 2:
                    self.pastes += 1
                    self.collapsed = True
                continue
            if self.buf.startswith(PASTE_START):
                self.buf = self.buf[len(PASTE_START):]
                self.in_paste = True
                continue
            if self.buf.startswith(b"\x1b") and len(self.buf) < len(PASTE_START) \
                    and PASTE_START.startswith(self.buf):
                return sent  # a paste marker cut in half
            ch, self.buf = self.buf[:1], self.buf[1:]
            if ch == b"\r":
                if self.drop_enters > 0:
                    self.drop_enters -= 1
                    self.draft += "\n"
                elif self.draft.endswith("\\"):
                    self.draft = self.draft[:-1] + "\n"
                elif self.draft.strip():
                    sent.append(self.draft)
                    self.draft, self.collapsed = "", False
            elif ch == b"\x0b":  # C-k
                self.draft, self.collapsed = "", False
            elif ch == b"\x1b":
                if self.buf.startswith(b"[") and len(self.buf) >= 2:
                    self.buf = self.buf[2:]  # an arrow key
            elif ch >= b" ":
                self.draft += ch.decode("utf-8", "replace")
        return sent

    def lines(self):
        first = self.draft.split("\n")[0]
        if self.collapsed:
            box = ["❯ ", f"  [Pasted text #{self.pastes} +{self.draft.count(chr(10))} lines]"]
        elif self.wrap and len(first) > self.wrap:
            box = ["❯ ", *("  " + row for row in textwrap.wrap(first, self.wrap,
                                                             break_long_words=False))]
        else:
            box = ["❯ " + first]
        return [RULE, *box, RULE, "  ⏵⏵ bypass permissions on"]


def draw(lines):
    sys.stdout.write("\x1b[H\x1b[2J" + "\r\n".join(lines))
    sys.stdout.flush()


class Raw:
    """stdin in raw mode, bracketed paste on, for the life of the block."""

    def __enter__(self):
        self.fd = sys.stdin.fileno()
        self.saved = termios.tcgetattr(self.fd)
        tty.setraw(self.fd)
        sys.stdout.write("\x1b[?2004h")
        sys.stdout.flush()
        return self

    def __exit__(self, *exc):
        termios.tcsetattr(self.fd, termios.TCSADRAIN, self.saved)

    def read(self, timeout):
        ready, _, _ = select.select([self.fd], [], [], timeout)
        return os.read(self.fd, 65536) if ready else b""


def main():
    out = sys.argv[1]
    boot = float(sys.argv[2]) if len(sys.argv) > 2 else 0.0
    composer = Composer(int(sys.argv[3]) if len(sys.argv) > 3 else 0,
                        int(sys.argv[4]) if len(sys.argv) > 4 else 0)
    with Raw() as term:
        draw(["booting…"])
        time.sleep(boot)
        draw(composer.lines())
        while True:
            for msg in composer.feed(term.read(1.0)):
                with open(out, "a") as f:
                    f.write(json.dumps({"text": msg}) + "\n")
            draw(composer.lines())


if __name__ == "__main__":
    main()
