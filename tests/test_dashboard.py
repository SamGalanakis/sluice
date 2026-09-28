"""The dashboard (SPEC §8 Views): pages over HTTP, the log viewer's pages and filters, and the
Datastar streams that patch a page only when what it shows has changed."""

import datetime as dt
import html
import json
import re
import socket
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

from sluice import util
from tests.conftest import create, d, message


def get(port, path, host=None):
    req = urllib.request.Request(f"http://127.0.0.1:{port}{path}",
                                 headers={"Host": host} if host else {})
    try:
        r = urllib.request.urlopen(req, timeout=10)
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
        s.sendall(f"GET {path}{query} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n"
                  f"Datastar-Request: true\r\nAccept: text/event-stream\r\n\r\n".encode())
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


def write_beat(home, beat):
    util.atomic_write_json(home / "runner.json",
                           {"pid": 1, "started": util.now_iso(), "beat": beat})


def fresh_beat():
    return dt.datetime.now(dt.UTC).strftime("%Y-%m-%dT%H:%M:%SZ")


# ---- the Host allowlist (the loopback trust boundary) --------------------------------------


def test_a_request_on_a_loopback_socket_must_be_addressed_to_this_machine(store, port):
    create(store, "p", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(2)}}})
    for host in ("evil.example:7420", "192.168.1.5:7420"):
        code, body = get(port, "/", host=host)
        assert code == 403 and "foreign Host" in body, host
    for host in (f"127.0.0.1:{port}", f"localhost:{port}", "[::1]"):
        assert get(port, "/", host=host)[0] == 200, host
    # a page, a stream, a static file and a POST all refuse it
    assert get(port, "/projects/p", host="evil.example:7420")[0] == 403
    assert get(port, "/static/sluice.js", host="evil.example:7420")[0] == 403
    with socket.create_connection(("127.0.0.1", port), timeout=10) as s:
        s.sendall(b"GET /stream HTTP/1.1\r\nHost: evil.example:7420\r\n"
                  b"Datastar-Request: true\r\n\r\n")
        assert s.recv(4096).startswith(b"HTTP/1.1 403")
    body = urllib.parse.urlencode({"paused": "1"}).encode()
    req = urllib.request.Request(f"http://127.0.0.1:{port}/projects/p/pause", data=body,
                                 method="POST",
                                 headers={"Host": "evil.example:7420",
                                          "Origin": "http://evil.example:7420",
                                          "content-type":
                                          "application/x-www-form-urlencoded"})
    try:
        urllib.request.urlopen(req, timeout=10)
        assert False, "a POST under a foreign Host was answered"
    except urllib.error.HTTPError as e:
        assert e.code == 403
    assert not store.paused("p")


# ---- pages ------------------------------------------------------------------------------


def test_every_page_renders(store, port):
    create(store, "p", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(2)}}})
    store.append("p", message("q", "hello"))
    code, index = get(port, "/")
    assert code == 200 and re.search(r'<a href="/projects/p"><span class="g g-pending".*?'
                                     r'<span>p</span></a>', index)
    assert "@get('/stream'" in index and '<div id="projects">' in index
    code, page = get(port, "/projects/p")
    assert code == 200 and '<div id="graph" tabindex="-1">' in page and "@get('/projects/p/stream'" in page
    assert '<a href="/projects/p/log">Log</a>' in page and 'data-step="a"' in page
    code, step = get(port, "/projects/p/steps/a")
    assert code == 200 and '<div id="step-detail">' in step and '<h2 id="d-title">a</h2>' in step
    assert "@get('/projects/p/steps/a/stream'" in step and '"sver"' in html.unescape(step)
    assert get(port, "/projects/p/steps/nope")[0] == 404
    assert get(port, "/projects/p/steps/nope/stream")[0] == 404
    code, fns = get(port, "/fns?project=p")
    assert code == 200 and "<b>test.add</b>" in fns
    assert '<a href="/projects/p/log">Log</a>' in fns  # the project's sections
    code, log = get(port, "/projects/p/log")
    assert code == 200 and "q from t: hello" in log and "@get('/projects/p/log/stream'" in log
    assert '<input type="checkbox" name="kind" value="step" data-bind:kinds>' in log
    code, home = get(port, "/log")
    assert code == 200 and "made without a project" in home and "No matching records." in home
    assert get(port, "/projects/nope")[0] == 404
    assert get(port, "/projects/nope/log")[0] == 404
    assert get(port, "/projects/nope/stream")[0] == 404
    assert get(port, "/projects/nope/threads/stream")[0] == 404
    assert get(port, "/projects/nope/inbox/stream")[0] == 404
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

    events = stream(port, "/projects/p/stream", {"ver": ver}, action=later(fail))
    sent = patches(events)
    graph = next(p for p in sent if p.startswith('elements <div id="graph" tabindex="-1">'))
    assert 'class="node card is-failed" id="n-a"' in graph
    assert "&lt;script&gt;alert(1)&lt;/script&gt;" in graph and "<script>alert" not in graph
    new_ver = [ev for ev in events if ev["event"] == "datastar-patch-signals"][-1]["data"]
    assert new_ver != [f'signals {{"ver":"{ver}"}}'] and new_ver[0].startswith('signals {"ver"')
    # a client with an old version gets every part at once, then nothing more
    stale = stream(port, "/projects/p/stream", {"ver": ver}, seconds=0.8)
    assert len(patches(stale)) == 4  # summary, graph, result and the nav badge


def test_the_threads_tab_streams_its_conversations(store, port):
    create(store, "p", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(2)}}})
    status, page = get(port, "/projects/p/threads")
    assert status == 200 and "No messages yet." in page
    ver = signals_of(page)["ver"]

    def post():
        store.append("p", {"kind": "message", "thread": "step-a", "from": "a",
                           "to": "orchestrator", "body": "Which <db>?"})

    sent = patches(stream(port, "/projects/p/threads/stream", {"ver": ver},
                          action=later(post)))
    threads = next(p for p in sent if p.startswith('elements <div id="threads">'))
    assert 'id="th-step-a"' in threads and "Which &lt;db&gt;?" in threads
    assert get(port, "/projects/nope/threads")[0] == 404


def test_a_running_steps_stderr_moves_its_progress_line(store, port):
    create(store, "p", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(2)}}})
    run = store.runs_dir("p") / "r1"
    run.mkdir(parents=True)
    log = run / "stderr.log"
    log.write_text("first\n")
    with store.lock("p"):
        store.write_state("p", {"inputs": {}, "steps": {"a": {
            "status": "running", "run_ids": ["r1"], "started": "2026-01-01T10:00:00Z"}}})
    ver = signals_of(get(port, "/projects/p")[1])["ver"]

    def write():
        with log.open("a") as f:
            f.write("second <b>line</b>\n")

    sent = patches(stream(port, "/projects/p/stream", {"ver": ver}, action=later(write)))
    [graph] = [p for p in sent if p.startswith('elements <div id="graph" tabindex="-1">')]
    assert 'title="second &lt;b&gt;line&lt;/b&gt;"' in graph  # the bubble's tooltip
    # the step's own stream (the drawer, or its page) follows the same file
    sver = signals_of(get(port, "/projects/p/steps/a")[1])["sver"]
    assert stream(port, "/projects/p/steps/a/stream", {"sver": sver}, seconds=0.8) == []

    def more():
        with log.open("a") as f:
            f.write("third\n")

    events = stream(port, "/projects/p/steps/a/stream", {"sver": sver}, action=later(more))
    [detail] = patches(events)
    assert detail.startswith('elements <div id="step-detail">') and "third" in detail
    assert events[-1]["data"][0].startswith('signals {"sver"')


def test_the_index_stream_shows_a_new_project(store, port):
    ver = signals_of(get(port, "/")[1])["ver"]
    assert stream(port, "/stream", {"ver": ver}, seconds=0.8) == []
    events = stream(port, "/stream", {"ver": ver},
                    action=later(lambda: store.create_project("fresh", "new one")))
    [table] = patches(events)
    assert re.search(r'<a href="/projects/fresh"><span class="g g-pending".*?<span>fresh</span>'
                     r'</a>', table) and "new one" in table


def test_the_runner_indicator_follows_the_heartbeat(store, port):
    """No runner.json and a fresh beat show nothing; a stale one says when the runner was last seen, live over the stream (the liveness is in `ver`,
    not the beat itself)."""
    create(store, "p", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(2)}}})
    write_beat(store.home, fresh_beat())
    index = get(port, "/")[1]
    assert "No runner is running" not in index and "Runner stopped" not in index
    write_beat(store.home, "2026-01-01T10:00:00Z")  # long past the 15 s
    code, index = get(port, "/")
    assert code == 200 and "Runner stopped · last seen" in index
    assert "Runner stopped · last seen" in get(port, "/projects/p")[1]
    (store.home / "runner.json").unlink()
    assert "Runner stopped" not in get(port, "/")[1]  # no heartbeat file is not evidence
    write_beat(store.home, "2026-01-01T10:00:00Z")  # stopped again; the stream sees it come back

    ver = signals_of(get(port, "/")[1])["ver"]

    def beat():
        write_beat(store.home, fresh_beat())

    sent = patches(stream(port, "/stream", {"ver": ver}, action=later(beat)))
    [projects] = [p for p in sent if p.startswith('elements <div id="projects">')]
    assert "Runner stopped" not in projects
    ver = signals_of(get(port, "/projects/p")[1])["ver"]

    def stop():
        write_beat(store.home, "2026-01-01T10:00:00Z")

    sent = patches(stream(port, "/projects/p/stream", {"ver": ver}, action=later(stop)))
    [summary] = [p for p in sent if p.startswith('elements <div id="summary">')]
    assert "Runner stopped · last seen" in summary


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


def test_the_log_stream_hides_thread_post_calls_too(store, port):
    store.create_project("p")
    sig = signals_of(get(port, "/projects/p/log")[1])

    def post():
        store.append("p", {"kind": "call", "call": "c1", "fn": "thread.post",
                           "status": "running", "direct": True},
                     message("q", "hello"),
                     {"kind": "call", "call": "c1", "fn": "thread.post",
                      "status": "succeeded"})

    [rows_patch] = patches(stream(port, "/projects/p/log/stream", sig, action=later(post)))
    assert "q from t: hello" in rows_patch and "thread.post" not in rows_patch


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

