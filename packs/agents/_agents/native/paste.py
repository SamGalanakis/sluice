# Adapted from Omnigent (https://github.com/omnigent-ai/omnigent),
# omnigent/harnesses/claude_native/bridge.py: inject_user_message, _paste_and_submit,
# _verify_submit_accepted, _restore_occupied_input, _submit_needle, _paste_payload_bytes.
# Copyright (2026) Databricks, Inc. Licensed under the Apache License, Version 2.0
# (http://www.apache.org/licenses/LICENSE-2.0). Changed: engine-agnostic (the pane reading is a
# composer object the engine adapter supplies), no web-UI prompt protection, plain exceptions.
"""Deliver a message into an engine's TUI composer through tmux, and verify it was sent.

The hard-won details, each a message lost in practice when missing:
- wait for the composer to render first (a message typed into a booting TUI is dropped);
- dismiss a surface covering the composer with Escape, but only while it is verifiably there
  (on a bare composer Escape interrupts the running turn);
- clear a leftover draft (C-a, C-k) before pasting;
- paste the text as ONE bracketed paste through a tmux buffer loaded from a file, never as
  send-keys argv (tmux caps a command at about 16 KB; interior newlines must not submit);
- end the paste with a newline, which absorbs a trailing backslash (else backslash + Enter
  reads as a line continuation and the message sits unsent);
- send Enter separately and verify it: wait until the draft is visible, press Enter, confirm
  the draft left the composer, and press Enter again while it has not (an Enter that lands
  while the TUI is still consuming the paste is folded into the draft)."""

import time

POLL = 0.15
READY_TIMEOUT = 180.0
COMMIT_TIMEOUT = 5.0  # for the pasted draft to show in the composer
SETTLE = 0.1  # between the draft showing and the Enter
VERIFY_TIMEOUT = 20.0  # for the draft to leave the composer
RETRY = 1.0  # first Enter retry; doubles up to RETRY_MAX
RETRY_MAX = 8.0
DISMISS_TIMEOUT = 3.0
DISMISS_RETRY = 0.75
NEEDLE_MAX = 24


class NotDelivered(RuntimeError):
    """The composer never took the message."""


def needle_of(text):
    """A short marker to spot the draft in the composer: the first non-empty line, cut at its
    first control character and to NEEDLE_MAX characters ("" when there is none)."""
    for line in text.replace("\r\n", "\n").replace("\r", "\n").split("\n"):
        for i, ch in enumerate(line):
            if ord(ch) < 0x20:
                line = line[:i]
                break
        line = line.strip()
        if line:
            return line[:NEEDLE_MAX]
    return ""


def payload(text):
    """The paste's bytes: every line break one CR (what a real paste carries), tabs kept,
    other control bytes dropped (a stray ESC would end the bracketed paste early)."""
    body = bytearray()
    for ch in text.replace("\r\n", "\n").replace("\r", "\n"):
        if ch == "\n":
            body.append(0x0D)
        elif ch == "\t":
            body.append(0x09)
        elif ord(ch) >= 0x20:
            body.extend(ch.encode("utf-8"))
    return bytes(body)


def tail(pane, lines=15, chars=1500):
    """The last lines of a pane, for an error message."""
    text = "\n".join([ln.rstrip() for ln in pane.splitlines() if ln.strip()][-lines:])
    return text[-chars:]


def restore(tmux, composer):
    """Dismiss whatever covers the composer (a menu, a search, a dialog) with Escape, only
    while it is verifiably on screen: seen on two polls in a row, re-sent while it stays."""
    deadline = time.monotonic() + DISMISS_TIMEOUT
    confirmed, last = False, None
    while time.monotonic() < deadline:
        pane = tmux.capture()
        if not pane.strip() or composer.occupied(pane) is None:
            return
        now = time.monotonic()
        if not confirmed:
            confirmed = True
        elif last is None or now - last >= DISMISS_RETRY:
            tmux.keys("Escape")
            last = now
        time.sleep(POLL)


def wait_ready(tmux, composer, timeout=READY_TIMEOUT, on_pane=None):
    """Block until the composer takes input. `on_pane(pane)` may act on what it sees (answer
    a startup dialog) or raise. Raises NotDelivered when the pane dies or time runs out."""
    deadline = time.monotonic() + timeout
    last = ""
    while True:
        pane = tmux.capture()
        if pane.strip():
            last = pane
        if composer.ready(pane):
            return
        if on_pane is not None:
            on_pane(pane)
        if tmux.dead() is not None:
            raise NotDelivered("the session exited before its input was ready. Last output:\n"
                               + tail(tmux.capture(history=200) or last))
        if time.monotonic() >= deadline:
            raise NotDelivered(f"the session's input never became ready in {timeout:.0f}s. "
                               "Last output:\n" + tail(last))
        time.sleep(POLL)


def deliver(tmux, text, composer, ready_timeout=READY_TIMEOUT):
    """Paste `text` into the composer and submit it; raises NotDelivered if it never leaves
    the composer."""
    restore(tmux, composer)
    wait_ready(tmux, composer, ready_timeout)
    needle = needle_of(text)
    tmux.keys("C-a")
    tmux.keys("C-k")
    tmux.paste(payload(text + "\n"))
    seen = False
    deadline = time.monotonic() + COMMIT_TIMEOUT
    while time.monotonic() < deadline:
        if composer.draft_visible(tmux.capture(), needle):
            seen = True
            break
        time.sleep(POLL)
    time.sleep(SETTLE)
    tmux.keys("Enter")
    if not seen:  # the draft was never identifiable, so its absence would prove nothing
        return
    start = last = time.monotonic()
    retry = RETRY
    while time.monotonic() - start < VERIFY_TIMEOUT:
        time.sleep(POLL)
        if not composer.draft_visible(tmux.capture(), needle):
            return
        now = time.monotonic()
        if now - last >= retry:
            tmux.keys("Enter")
            last, retry = now, min(retry * 2, RETRY_MAX)
    raise NotDelivered(f"the message was pasted but never left the input box in "
                       f"{VERIFY_TIMEOUT:.0f}s. Last output:\n" + tail(tmux.capture()))
