"""The dashboard (SPEC §8 Views): pages over HTTP, the log viewer's pages and filters, and the
Datastar streams that patch a page only when what it shows has changed."""

import html
import json
import re
import socket
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

from tests.conftest import create


def d(x):
    return {"default": x}


def message(thread, body, frm="t"):
    return {"kind": "message", "thread": thread, "from": frm, "body": body}


def get(port, path):
    try:
        r = urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=10)
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()
    return r.status, r.read().decode()


def signals_of(page):
    return json.loads(html.unescape(re.search(r'<body data-signals="([^"]*)"', page)[1]))


def stream(port, path, signals=None, seconds=1.5, action=None):
    """Read an SSE stream for `seconds` (calling `action` once connected) and return its
    events as {"event": type, "data": [lines]}."""
    query = ("?datastar=" + urllib.parse.quote(json.dumps(signals))) if signals is not None \
        else ""
    with socket.create_connection(("127.0.0.1", port), timeout=10) as s:
        s.sendall(f"GET {path}{query} HTTP/1.1\r\nHost: x\r\nDatastar-Request: true\r\n"
                  f"Accept: text/event-stream\r\n\r\n".encode())
        data = b""
        while b"\r\n\r\n" not in data:
            data += s.recv(65536)
        head, data = data.split(b"\r\n\r\n", 1)
        assert head.startswith(b"HTTP/1.1 200"), head
        assert b"text/event-stream" in head
        if action:
            action()
        deadline = time.time() + seconds
        while (left := deadline - time.time()) > 0:
            s.settimeout(left)
            try:
                chunk = s.recv(65536)
            except TimeoutError:
                break
            if not chunk:
                break
            data += chunk
    text = data.decode()
    # undo chunked transfer encoding: drop the chunk-size lines
    text = re.sub(r"(?m)^[0-9a-fA-F]+\r\n", "", text).replace("\r\n", "")
    events = []
    for block in text.split("\n\n"):
        lines = [ln for ln in block.split("\n") if ln]
        if lines and lines[0].startswith("event: "):
            events.append({"event": lines[0][7:], "data": [ln[6:] for ln in lines[1:]]})
    return events


def patches(events):
    return ["\n".join(ev["data"]) for ev in events if ev["event"] == "datastar-patch-elements"]


def later(fn, delay=0.4):
    return lambda: threading.Timer(delay, fn).start()


# ---- pages ------------------------------------------------------------------------------


def test_every_page_renders(store, port):
    create(store, "p", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(2)}}})
    store.append("p", message("q", "hello"))
    code, index = get(port, "/")
    assert code == 200 and '<a href="/projects/p">p</a>' in index
    assert "@get('/stream'" in index and '<div id="projects">' in index
    code, page = get(port, "/projects/p")
    assert code == 200 and '<div id="steps">' in page and "@get('/projects/p/stream'" in page
    assert '<a href="/projects/p/log">' in page and "q from t: hello" in page
    code, fns = get(port, "/fns?project=p")
    assert code == 200 and "<b>test.add</b>" in fns and '<a href="/log">Log</a>' in fns
    code, log = get(port, "/projects/p/log")
    assert code == 200 and "q from t: hello" in log and "@get('/projects/p/log/stream'" in log
    assert '<input type="checkbox" name="kind" value="step" data-bind:kinds>' in log
    code, home = get(port, "/log")
    assert code == 200 and "calls without a project" in home and "No matching records." in home
    assert get(port, "/projects/nope")[0] == 404
    assert get(port, "/projects/nope/log")[0] == 404
    assert get(port, "/projects/nope/stream")[0] == 404
    assert get(port, "/projects/p/log?kind=bogus")[0] == 400
    assert get(port, "/projects/p/log?before=x")[0] == 400
    assert get(port, "/projects/p/log?before=3&after=1")[0] == 400


def rows(page):
    return [int(x) for x in re.findall(r'<tr id="r(\d+)">', page)]


def links(page):
    nav = re.search(r'<nav class="pager">(.*?)</nav>', page)[1]
    return {text: html.unescape(href) for href, text in
            re.findall(r'<a href="([^"]*)">([^<]*)</a>', nav)}


def test_the_log_pages_by_seq_newest_first(store, port):
    for i in range(1, 121):
        store.append(None, message("q", f"m{i}"))
    code, page = get(port, "/log")
    assert code == 200 and rows(page) == list(range(120, 70, -1))
    assert links(page) == {"older ›": "/log?before=71"}
    older = get(port, "/log?before=71")[1]
    assert rows(older) == list(range(70, 20, -1))
    assert links(older) == {"« newest": "/log", "‹ newer": "/log?after=70",
                            "older ›": "/log?before=21"}
    oldest = get(port, "/log?before=21")[1]
    assert rows(oldest) == list(range(20, 0, -1)) and "older ›" not in links(oldest)
    assert rows(get(port, "/log?before=51")[1]) == list(range(50, 0, -1))
    empty = get(port, "/log?before=1")[1]
    assert rows(empty) == [] and "No matching records." in empty
    newer = get(port, "/log?after=20")[1]
    assert rows(newer) == list(range(70, 20, -1))
    assert links(newer) == {"« newest": "/log", "‹ newer": "/log?after=70",
                            "older ›": "/log?before=21"}
    top = get(port, "/log?after=70")[1]
    assert rows(top) == list(range(120, 70, -1)) and "‹ newer" not in links(top)


def test_the_log_filters_by_kind_prefix_and_thread(store, port):
    store.create_project("p")  # seq 1: plan.edit
    for i in range(2, 122):
        store.append("p", message("a" if i % 2 else "b", f"m{i}") if i % 3
                     else {"kind": "step.status", "step": "s", "from": None, "to": "pending"})

    def seqs(query):
        code, page = get(port, "/projects/p/log" + query)
        assert code == 200
        return rows(page), page

    steps, _ = seqs("?kind=step")
    assert steps == list(range(120, 1, -3))
    plan, _ = seqs("?kind=plan")
    assert plan == [1]
    a, page = seqs("?thread=a")
    assert a == [i for i in range(121, 1, -1) if i % 2 and i % 3]
    both, page = seqs("?kind=step&kind=message&thread=b")
    assert both == [i for i in range(121, 1, -1) if i % 3 == 0 or i % 2 == 0][:50]
    assert links(page) == {"older ›": f"/projects/p/log?kind=step&kind=message&thread=b"
                                      f"&before={both[-1]}"}
    rest, page = seqs(f"?kind=step,message&thread=b&before={both[-1]}")
    assert rest == [i for i in range(both[-1] - 1, 1, -1) if i % 3 == 0 or i % 2 == 0]
    assert signals_of(page)["kinds"][3] == "step" and signals_of(page)["thread"] == "b"


# ---- streams ----------------------------------------------------------------------------


def test_the_project_stream_patches_only_after_a_change(store, port):
    create(store, "p", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(2)}}})
    page = get(port, "/projects/p")[1]
    ver = signals_of(page)["ver"]
    assert stream(port, "/projects/p/stream", {"ver": ver}, seconds=1.0) == []  # idle

    def fail():
        with store.lock("p"):
            store.write_state("p", {"inputs": {}, "steps": {
                "a": {"status": "failed", "error": "<script>alert(1)</script>"}}})
        store.append("p", message("q", "<script>alert(2)</script>"))

    events = stream(port, "/projects/p/stream", {"ver": ver}, action=later(fail))
    sent = patches(events)
    steps = next(p for p in sent if p.startswith('elements <div id="steps">'))
    assert '<td class="s-failed">failed</td>' in steps
    assert "&lt;script&gt;alert(1)&lt;/script&gt;" in steps and "<script>alert" not in steps
    diagram = next(p for p in sent if 'id="plan-src"' in p)
    assert diagram.startswith("mode replace\n") and "a / test.add / failed" in diagram
    recent = next(p for p in sent if p.startswith('elements <div id="recent">'))
    assert "&lt;script&gt;alert(2)&lt;/script&gt;" in recent and "<script>alert" not in recent
    assert not any(p.startswith('elements <div id="inputs">') for p in sent)  # unchanged
    new_ver = [ev for ev in events if ev["event"] == "datastar-patch-signals"][-1]["data"]
    assert new_ver != [f'signals {{"ver":"{ver}"}}'] and new_ver[0].startswith('signals {"ver"')
    # a client with an old version gets every part at once, then nothing more
    stale = stream(port, "/projects/p/stream", {"ver": ver}, seconds=0.8)
    assert len(patches(stale)) == 8  # summary, diagram, inputs, outputs, steps, history,
    #                                   recent log and the nav badge


def test_the_index_stream_shows_a_new_project(store, port):
    ver = signals_of(get(port, "/")[1])["ver"]
    assert stream(port, "/stream", {"ver": ver}, seconds=0.8) == []
    events = stream(port, "/stream", {"ver": ver},
                    action=later(lambda: store.create_project("fresh", "new one")))
    [table] = patches(events)
    assert '<a href="/projects/fresh">fresh</a>' in table and "new one" in table


def test_the_log_stream_prepends_new_matching_records_on_the_newest_page(store, port):
    store.create_project("p")
    page = get(port, "/projects/p/log?kind=message&thread=q")[1]
    sig = signals_of(page)
    assert sig["view"] == "kind=message&thread=q" and sig["seen"] == 1
    assert stream(port, "/projects/p/log/stream", sig, seconds=1.0) == []  # idle

    def post():
        store.append("p", message("other", "not shown"))
        store.append("p", message("q", "<script>alert(1)</script>"))

    events = stream(port, "/projects/p/log/stream", sig, action=later(post))
    [rows_patch] = patches(events)
    assert rows_patch.startswith("mode prepend\nselector #log-rows\nelements <tr id=\"r3\">")
    assert "q from t: &lt;script&gt;alert(1)&lt;/script&gt;" in rows_patch
    assert "not shown" not in rows_patch and "<script>alert" not in rows_patch
    assert events[-1]["data"] == ['signals {"seen":3}']
    # other records only move `seen` on
    sig["seen"] = 3
    events = stream(port, "/projects/p/log/stream", sig,
                    action=later(lambda: store.append("p", message("other", "x"))))
    assert patches(events) == [] and events[-1]["data"] == ['signals {"seen":4}']


def test_the_log_stream_sends_the_table_when_the_filter_changes(store, port):
    store.create_project("p")
    store.append("p", message("q", "hello"), message("r", "other"))
    sig = signals_of(get(port, "/projects/p/log")[1])
    assert sig["view"] == "" and sig["seen"] == 3
    sig["thread"] = "r"  # as the thread input would set it
    events = stream(port, "/projects/p/log/stream", sig, seconds=0.8)
    [view] = patches(events)
    assert view.startswith('elements <div id="log-view"')
    assert "r from t: other" in view and "hello" not in view
    assert events[-1]["data"] == ['signals {"view":"thread=r","seen":3}']
    older = dict(sig, before=3, view="")  # an older page: the table, then no streaming
    events = stream(port, "/projects/p/log/stream", older,
                    action=later(lambda: store.append("p", message("r", "late"))))
    assert len(patches(events)) == 1 and "late" not in patches(events)[0]
