"""The dashboard's inbox (SPEC §8): its pages, the badge, the answer route (the dashboard's one
write, through the same Store.inbox_answer as the MCP tool), and the OpenUI vocabulary shared
by the renderer and the agent docs; with a Chromium, the renderer itself and the board's step
drawer."""

import json
import re
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

import pytest

from sluice.mcp_server import DOCS
from tests.browser import Chrome, find_chrome
from tests.conftest import create
from tests.test_dashboard import get, later, patches, signals_of, stream

STATIC = Path(__file__).resolve().parents[1] / "src" / "sluice" / "static"
VOCAB = json.loads((STATIC / "openui.json").read_text())


def post(port, path, data, json_body=True, headers=None):
    body = json.dumps(data).encode() if json_body else urllib.parse.urlencode(data).encode()
    kind = "application/json" if json_body else "application/x-www-form-urlencoded"
    req = urllib.request.Request(f"http://127.0.0.1:{port}{path}", data=body, method="POST",
                                 headers={"content-type": kind, **(headers or {})})

    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *args, **kwargs):
            return None

    try:
        r = urllib.request.build_opener(NoRedirect).open(req, timeout=10)
    except urllib.error.HTTPError as e:
        return e.code, e.headers, e.read().decode()
    return r.status, r.headers, r.read().decode()


def badge(page):
    m = re.search(r'<a id="nav-inbox" href="/inbox">Inbox(?: <span class="badge"[^>]*>(\d+)'
                  r"</span>)?</a>", page)
    assert m, "no inbox link in the nav"
    return int(m[1] or 0)


# ---- pages ------------------------------------------------------------------------------


def test_inbox_pages_and_the_badge(store, port):
    create(store, "p", {})
    store.create_project("q")
    assert badge(get(port, "/")[1]) == 0
    a = store.inbox_post("p", "First?", sender="plan")["id"]
    store.inbox_post("q", "Second?")
    for path in ("/", "/projects/p", "/fns", "/log", "/projects/p/log", "/inbox"):
        code, page = get(port, path)
        assert code == 200 and badge(page) == 2, path
    assert page.count('class="badge"') == 1  # the one red badge
    code, page = get(port, "/inbox")
    assert "First?" in page and "Second?" in page and '<script type="module" ' \
        'src="/static/inbox.js">' in page and "@get('/inbox/stream'" in page
    assert '<a href="/projects/p/inbox">p</a>' in page and "from plan" in page
    code, page = get(port, "/projects/p/inbox")
    assert code == 200 and "First?" in page and "Second?" not in page
    assert '<form method="post" action="/projects/p/inbox/i1/answer">' in page
    assert get(port, "/projects/nope/inbox")[0] == 404
    assert get(port, "/projects/nope/inbox/stream")[0] == 404
    assert get(port, "/inbox?status=bogus")[0] == 400

    store.inbox_answer("p", a, {"action": "answer", "text": "yes"}, "me")
    assert badge(get(port, "/projects/p")[1]) == 1
    assert "First?" not in get(port, "/projects/p/inbox")[1]
    answered = get(port, "/projects/p/inbox?status=answered")[1]
    assert "First?" in answered and "<blockquote>yes</blockquote>" in answered
    assert "First?" in get(port, "/inbox?status=all")[1]
    assert "Nothing is waiting on you." in get(port, "/projects/p/inbox")[1]


def test_everything_from_an_item_is_escaped(store, port):
    store.create_project("p")
    x = "<script>alert(1)</script>"
    i = store.inbox_post("p", f"title {x}", body=f"body {x}\n\n<img src=a onerror=alert(2)>"
                         f"\n\n[link](javascript:alert(3))", ui=f'root = Text("{x}")',
                         sender=x)["id"]
    page = get(port, "/inbox")[1]
    assert "<script>alert" not in page and "<img" not in page
    assert "title &lt;script&gt;alert(1)&lt;/script&gt;" in page
    assert "<p>body &lt;script&gt;alert(1)&lt;/script&gt;</p>" in page
    assert 'href="javascript' not in page
    assert 'data-ui="root = Text(&quot;&lt;script&gt;' in page
    store.inbox_answer("p", i, {"action": x, "text": x, "values": {"v": x}}, "me")
    page = get(port, "/inbox?status=answered")[1]
    assert "<script>alert" not in page and "&lt;script&gt;alert(1)&lt;/script&gt;" in page


def test_markdown_bodies_render(store, port):
    store.create_project("p")
    store.inbox_post("p", "q", body="Some **bold**, `code` and\n\n| a | b |\n|---|---|\n| 1 | 2 |")
    page = get(port, "/inbox")[1]
    assert "<strong>bold</strong>" in page and "<code>code</code>" in page
    assert "<th>a</th>" in page and "<td>2</td>" in page


# ---- answering ----------------------------------------------------------------------------


def test_the_form_answers_through_the_store_and_a_second_post_is_refused(store, port):
    create(store, "p", {}, inputs={"who": "string"})
    i = store.inbox_post("p", "Who?", input="who")["id"]
    url = f"/projects/p/inbox/{i}/answer"
    code, headers, _ = post(port, url, {"text": "Sam", "next": "/projects/p/inbox"},
                            json_body=False)
    assert code == 303 and headers["location"] == "/projects/p/inbox"
    item = store.inbox("p", "answered")[0]
    assert item["answer"] == {"action": "answer", "text": "Sam"}
    assert store.read_state("p")["inputs"] == {"who": "Sam"}
    rec = [r for r in store.history("p") if r["kind"] == "plan.input"][-1]
    assert (rec["author"], rec["reason"]) == ("dashboard", f"inbox item {i}: Who?")
    code, _, body = post(port, url, {"text": "Kim"}, json_body=False)
    assert code == 409 and "is answered, not open" in body
    code, _, body = post(port, url, {"action": "answer", "text": "Kim"})
    assert code == 409 and json.loads(body)["status"] == "answered"
    assert store.read_state("p")["inputs"] == {"who": "Sam"}
    code, headers, _ = post(port, url, {"text": "x", "next": "//evil.example"},
                            json_body=False)
    assert code == 409


def test_json_answers_and_their_refusals(store, port):
    create(store, "p", {}, inputs={"n": "int"})
    i = store.inbox_post("p", "How many?", input="n")["id"]
    url = f"/projects/p/inbox/{i}/answer"
    code, _, body = post(port, url, {"action": "submit", "values": {"value": "three"}})
    assert code == 400 and json.loads(body)["errors"] == ['inputs.n: expected int, got "three"']
    code, _, body = post(port, url, {"values": {"value": 3}})
    assert code == 400 and "answer.action" in body
    code, _, _ = post(port, url, {"action": "submit", "values": {"value": 3}},
                      headers={"origin": "http://evil.example"})
    assert code == 403
    assert store.inbox("p")[0]["status"] == "open"  # nothing above answered it
    code, _, body = post(port, url, {"action": "submit", "params": {}, "values": {"value": 3}})
    assert code == 200 and json.loads(body)["status"] == "answered"
    assert store.read_state("p")["inputs"] == {"n": 3}
    code, _, body = post(port, "/projects/p/inbox/i9/answer", {"action": "x"})
    assert code == 404
    assert post(port, "/projects/nope/inbox/i1/answer", {"action": "x"})[0] == 404


def test_the_inbox_stream_drops_an_answered_item_and_moves_the_badge(store, port):
    store.create_project("p")
    i = store.inbox_post("p", "Pick one")["id"]
    ver = signals_of(get(port, "/inbox")[1])["ver"]
    assert stream(port, "/inbox/stream", {"ver": ver, "status": "open"}, seconds=0.8) == []
    events = stream(port, "/inbox/stream", {"ver": ver, "status": "open"},
                    action=later(lambda: store.inbox_answer("p", i, {"action": "a"}, "me")))
    sent = patches(events)
    items = next(p for p in sent if p.startswith('elements <div id="inbox-items">'))
    assert "Pick one" not in items and "Nothing is waiting on you." in items
    assert 'elements <a id="nav-inbox" href="/inbox">Inbox</a>' in sent
    ver = signals_of(get(port, "/")[1])["ver"]
    events = stream(port, "/stream", {"ver": ver},
                    action=later(lambda: store.inbox_post("p", "Another")))
    assert any('<span class="badge" title="open items">1</span>' in p for p in patches(events))


def test_static_files(port):
    code, js = get(port, "/static/inbox.js")
    assert code == 200 and "createParser" in js
    code, vocab = get(port, "/static/openui.json")
    assert code == 200 and json.loads(vocab) == VOCAB
    assert get(port, "/static/../views.py")[0] == 404
    assert get(port, "/static/nope.js")[0] == 404


# ---- the OpenUI vocabulary ------------------------------------------------------------------


def signature(component):
    return f"{component['name']}({', '.join(f'{n}: {t}' for n, t in component['props'])})"


def test_the_agent_docs_list_exactly_the_vocabulary():
    page = (DOCS / "inbox.md").read_text()
    section = page.split("<!-- vocabulary:", 1)[1].split("<!-- end vocabulary -->")[0]
    listed = re.findall(r"^- `(.*?)` — (.*)$", section, re.MULTILINE)
    assert listed == [(signature(c), c["description"]) for c in VOCAB["components"]]


def test_the_renderer_draws_exactly_the_vocabulary():
    js = (STATIC / "inbox.js").read_text()
    block = js.split("const RENDERERS = {", 1)[1].split("\n};", 1)[0]
    assert re.findall(r"^  (\w+):", block, re.MULTILINE) == [c["name"] for c in VOCAB["components"]]
    assert re.search(r'"https://cdn\.jsdelivr\.net/npm/@openuidev/lang-core@\d+\.\d+\.\d+/\+esm"',
                     js), "lang-core must be pinned to an exact version"


def examples():
    return re.findall(r"```openui\n(.*?)```", (DOCS / "inbox.md").read_text(), re.DOTALL)


def test_the_doc_examples_use_only_the_vocabulary():
    names = {c["name"] for c in VOCAB["components"]}
    assert len(examples()) == 3
    for program in examples():
        used = set(re.findall(r"\b([A-Z]\w*)\(", program))
        assert used and used <= names, used - names


# ---- the renderer in a browser ------------------------------------------------------------

CHROME = find_chrome()
LANG_CORE = re.search(r'"(https://cdn\.jsdelivr\.net/npm/@openuidev/lang-core@[^"]+)"',
                      (STATIC / "inbox.js").read_text())[1]


def cdn_reachable() -> bool:
    try:
        urllib.request.urlopen(LANG_CORE, timeout=10).close()
    except OSError:
        return False
    return True


@pytest.fixture
def chrome():
    if CHROME is None:
        pytest.skip("no Chromium (set SLUICE_CHROME)")
    if not cdn_reachable():
        pytest.skip("cdn.jsdelivr.net is not reachable")
    c = Chrome(CHROME)
    yield c
    c.close()


def click(c, item, label):
    c.eval(f"[...document.querySelectorAll('#item-p-{item} button')]"
           f".find(b => b.textContent === {json.dumps(label)}).click()")


def test_the_doc_examples_render_and_their_buttons_answer(store, port, chrome):
    create(store, "p", {}, inputs={"approved": "boolean"})
    approve, pick, form = examples()
    a = store.inbox_post("p", "Approve?", ui=approve, input="approved")["id"]
    b = store.inbox_post("p", "Which database?", ui=pick)["id"]
    f = store.inbox_post("p", "Release?", ui=form)["id"]
    broken = store.inbox_post("p", "Broken", ui='root = Stack([ok, gone])\nok = Text("kept")\n'
                              'gone = Hologram("x")\njunk line')["id"]
    chrome.open(f"http://127.0.0.1:{port}/projects/p/inbox")
    chrome.wait("document.querySelectorAll('.ou-root').length === 4")
    assert chrome.eval("window.sluiceOpenUI.components.join()") == \
        ",".join(c["name"] for c in VOCAB["components"])
    for item in (a, b, f):  # the examples draw completely
        assert chrome.eval(f"document.querySelector('#item-p-{item} .ou-dropped')") is None
    assert chrome.eval(f"document.querySelector('#item-p-{a} td').textContent") == \
        "billing: retry failed charges"
    # a broken program keeps what it can, says what it dropped, and keeps the text box
    assert chrome.eval(f"document.querySelector('#item-p-{broken} .ou-root p').textContent") \
        == "kept"
    assert chrome.eval(f"document.querySelector('#item-p-{broken} .ou-dropped summary')"
                       ".textContent") == "2 lines dropped"
    assert chrome.eval(f"!!document.querySelector('#item-p-{broken} .answer > form "
                       "textarea')")
    # with buttons, the text box is folded away
    assert chrome.eval(f"!!document.querySelector('#item-p-{a} details.ou-words form')")

    click(chrome, a, "Approve")
    chrome.wait(f"document.querySelector('#item-p-{a}') === null")  # the stream removed it
    got = {i["id"]: i for i in store.inbox("p", "all")}
    assert got[a]["answer"] == {"action": "approve", "params": {"value": True}, "values": {}}
    assert store.read_state("p")["inputs"] == {"approved": True}

    click(chrome, b, "Choose")  # required: nothing is sent yet
    assert chrome.wait(f"document.querySelector('#item-p-{b} .ou-error:not([hidden])')"
                       "?.textContent") == "This field is required"
    chrome.eval(f"document.querySelector('#item-p-{b} input[value=sqlite]').click()")
    click(chrome, b, "Choose")
    chrome.wait(f"document.querySelector('#item-p-{b}') === null")
    assert store.inbox("p", "answered")[-1]["answer"] == {
        "action": "choose", "params": {}, "values": {"value": "sqlite"}}

    chrome.eval(f"document.querySelector('#item-p-{f} textarea[name=notes]').value = "
                "'Retries failed charges.'")
    click(chrome, f, "Ship")
    chrome.wait(f"document.querySelector('#item-p-{f}') === null")
    assert store.inbox("p", "answered")[-1]["answer"] == {
        "action": "ship", "params": {},
        "values": {"version": "1.4.0", "notes": "Retries failed charges.", "notify": True}}
    assert [i["id"] for i in store.inbox("p")] == [broken]


# ---- the board in a browser----------------------------------------------------------------------


def test_a_card_opens_the_step_drawer_and_escape_closes_it(store, port, chrome):
    one, two = {"default": 1}, {"default": 2}
    create(store, "p", {"a": {"run": "test.add", "in": {"a": one, "b": two}, "doc": "First"},
                        "b": {"run": "test.add", "in": {"a": {"source": "a/sum"}, "b": two},
                              "doc": "Second"}})
    chrome.open(f"http://127.0.0.1:{port}/projects/p")
    chrome.wait("!!window.sluiceStream && !!document.querySelector('#n-a')")
    chrome.eval("document.querySelector('#n-a').click()")
    assert chrome.wait("document.querySelector('#step-detail h2')?.textContent") == "First"
    assert chrome.eval("location.hash") == "#step:a"
    assert chrome.eval("document.querySelector('#n-a').classList.contains('open')")
    # tracing: focusing b lights the edge from a
    chrome.eval("document.querySelector('#n-b').focus()")
    assert chrome.eval("document.querySelector('path[data-from=\"s:a\"][data-to=\"s:b\"]')"
                       ".classList.contains('on')")
    chrome.eval("document.querySelector('#n-b').click()")
    chrome.wait("document.querySelector('#step-detail h2')?.textContent === 'Second'")
    chrome.eval("document.dispatchEvent(new KeyboardEvent('keydown', {key: 'Escape'}))")
    chrome.wait("getComputedStyle(document.getElementById('drawer')).display === 'none'")
    assert chrome.eval("location.hash") == ""
