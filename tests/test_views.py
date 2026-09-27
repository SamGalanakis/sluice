"""The dashboard's views (SPEC §8): the Mermaid text plan_view gives agents, the board of step
cards and its layout, a step's detail, the "Needs you" lines, the index, and escaping."""

import html
import json
import re

from sluice import log as L
from sluice import views
from tests.conftest import create, write_fn


def d(x):
    return {"default": x}


def src(ref):
    return {"source": ref}


def test_mermaid_shows_inputs_steps_outputs_edges_and_status_classes(store):
    create(store, "v", {
        "a": {"run": "test.add", "in": {"a": {"source": "n"}, "b": d(1)}},
        "b": {"run": "test.add", "in": {"a": {"source": "a/sum"}, "b": d(2)}},
        "c": {"run": "test.boom", "in": {}},
        "each": {"run": "test.window", "scatter": "tag",
                 "in": {"seconds": d(0), "tag": {"source": ["a/sum"]}}},
        "gate": {"run": "core.collect", "in": {"items": {"source": ["b/sum", "each/end"]}}},
    }, inputs={"n": "int"}, outputs={"all": {"source": "gate/items"}})
    with store.lock("v"):
        store.write_state("v", {"inputs": {"n": 1}, "steps": {
            "a": {"status": "succeeded", "outputs": {"sum": 2}},
            "b": {"status": "succeeded", "outputs": {"sum": 4}, "manual": True},
            "c": {"status": "failed", "error": "exit code 1"},
            "each": {"status": "running", "done": 1, "total": 3},
        }})
    text = views.render(store, "v", "mermaid")
    lines = text.splitlines()
    assert lines[0] == "flowchart LR"
    node = {m[1]: m[0] for m in re.findall(r'^\s+(\w+)[\[(]+"([^"]+)"', text, re.MULTILINE)}
    assert set(node) == {"n", "a / test.add / succeeded", "b / test.add / succeeded",
                         "c / test.boom / failed", "each / test.window / running 1/3",
                         "gate / core.collect / pending", "all"}
    assert f'  {node["n"]}(["n"])' in lines and f'  {node["all"]}(["all"])' in lines
    a, b, each, gate = (node[k] for k in ("a / test.add / succeeded", "b / test.add / succeeded",
                                          "each / test.window / running 1/3",
                                          "gate / core.collect / pending"))
    edges = {ln.strip() for ln in lines if "-->" in ln}
    assert edges == {f'{node["n"]} -->|"n"| {a}', f'{a} -->|"sum"| {b}', f'{a} -->|"sum"| {each}',
                     f'{b} -->|"sum"| {gate}', f'{each} -->|"end"| {gate}',
                     f'{gate} -->|"items"| {node["all"]}'}
    classes = dict(re.findall(r"^\s+class (\w+) (\w+)$", text, re.MULTILINE))
    assert classes == {a: "succeeded", b: "manual", node["c / test.boom / failed"]: "failed",
                       each: "running", gate: "pending"}
    for cls in ("pending", "running", "succeeded", "failed", "manual"):
        assert f"  classDef {cls} " in text


# ---- the board ----------------------------------------------------------------------------


def card(page, sid):
    """The card of a step on the board: its element, up to its end."""
    m = re.search(rf'<(a|div) class="node (card|chip) [^"]*" id="n-{sid}".*?</\1>', page,
                  re.DOTALL)
    assert m, f"no card for {sid}"
    return m[0]


def rows(page):
    """The step ids on the board, row by row."""
    return [re.findall(r'id="n-([^"]+)"', r)
            for r in re.findall(r'<li class="row"[^>]*>(.*?)</li>', page, re.DOTALL)]


def board_project(store):
    create(store, "v", {
        "a": {"run": "test.add", "in": {"a": src("n"), "b": d(1)}, "doc": "Add one to n"},
        "fmt": {"run": "core.format", "in": {"template": d("{0}"), "values": src(["a/sum"])}},
        "b": {"run": "test.add", "in": {"a": src("a/sum"), "b": d(2)}},
        "c": {"run": "test.boom", "in": {}},
        "late": {"run": "test.add", "in": {"a": src("a/sum"), "b": src("b/sum")}},
        "each": {"run": "test.window", "scatter": "tag",
                 "in": {"seconds": d(0), "tag": src(["a/sum"])}},
    }, inputs={"n": "int"}, outputs={"total": src("b/sum")})
    run = store.runs_dir("v") / "r1"
    run.mkdir(parents=True)
    (run / "input.json").write_text('{"seconds": 0, "tag": 2}')
    (run / "stderr.log").write_text("starting\nhalfway there\n\n")
    with store.lock("v"):
        store.write_state("v", {"inputs": {"n": 1}, "steps": {
            "a": {"status": "succeeded", "outputs": {"sum": 2}, "started": "2026-01-01T10:00:00Z",
                  "finished": "2026-01-01T10:12:04Z"},
            "fmt": {"status": "succeeded", "outputs": {"text": "2"}},
            "b": {"status": "succeeded", "outputs": {"sum": 4}, "manual": True},
            "c": {"status": "failed", "error": "exit code 1\ntraceback <here>"},
            "each": {"status": "running", "done": 1, "total": 3, "run_ids": ["r1"],
                     "started": "2026-01-01T10:12:05Z"},
            "late": {"status": "stale", "outputs": {"sum": 6}}}})


def test_the_board_shows_each_step_as_a_bubble(store):
    board_project(store)
    page = views.project_page(store, "v", ver="abc")
    a = card(page, "a")
    assert a.startswith('<a class="node card is-succeeded" id="n-a" data-node="s:a" '
                        'href="/projects/v/steps/a" data-step="a" title="Add one to n">')
    assert '<span class="vh">succeeded</span>' in a  # the glyph's word, for assistive tech
    assert '<span class="sid">a</span><span class="dur">12m 4s</span>' in a
    # just the name and, small, its time: outputs, engine and cost are in the drawer
    assert "sum" not in a and "test.add" not in a and "$" not in a
    assert "title=" not in card(page, "b").split(">", 1)[0]  # no doc, nothing to say
    assert "is-manual" in card(page, "b")
    c = card(page, "c")  # failed: its error is the tooltip
    assert "is-failed" in c and 'title="exit code 1…"' in c
    each = card(page, "each")  # running: its progress is the tooltip, done/total beside it
    assert "is-running" in each and 'title="halfway there"' in each and "1/3" in each
    assert 'data-since="2026-01-01T10:12:05Z"' in each  # its running time stays current
    assert "is-stale" in card(page, "late") and "Its inputs changed" in card(page, "late")
    assert card(page, "fmt").startswith('<a class="node chip is-succeeded"')  # glue: dashed
    # the head is the project; the plan's result, its inputs, counts and cost follow the
    # board (plan inputs and outputs are not board nodes)
    facts = page[page.index('<section id="result" class="plan-facts">'):]
    assert page.index('id="graph"') < page.index('id="result"')
    assert "<dt>total</dt><dd><code class=\"v\">4</code></dd>" in facts
    assert "<dt>n</dt><dd><code class=\"v\">1</code></dd>" in facts
    assert 'data-node="o:' not in page and 'data-node="i:' not in page
    # the page: its counts, the drawer that shows a step, and the live stream
    assert "3 of 6 succeeded · 1 running · 1 stale · 1 failed" in facts
    assert 'id="drawer"' in page
    assert "'/projects/v/steps/' + encodeURIComponent($step)" in html.unescape(page)
    assert "data-init=\"@get('/projects/v/stream', {retry: 'always'" in page
    assert '<script type="module" src="/static/board.js">' in page
    assert "mermaid" not in page


def test_the_board_lays_steps_out_in_rows_by_dependency_depth(store):
    board_project(store)
    page = views.project_page(store, "v", ver="abc")
    assert rows(page) == [["a", "c"], ["fmt", "b", "each"], ["late"]]
    assert '<li class="row" style="--n:3">' in page
    # the edges, one per handoff, named by their ports, for board.js to draw
    data = json.loads(html.unescape(re.search(r'<div class="plane" data-edges="([^"]*)"',
                                              page)[1]))
    assert {(f, t): n for f, t, n in data} == {
        ("s:a", "s:fmt"): "sum → values", ("s:a", "s:b"): "sum → a",
        ("s:a", "s:each"): "sum → tag", ("s:a", "s:late"): "sum → a",
        ("s:b", "s:late"): "sum → b"}
    # more than four side by side wrap inside their row
    create(store, "wide", {f"s{i}": {"run": "test.add", "in": {"a": d(i), "b": d(1)}}
                           for i in range(6)})
    assert '<li class="row" style="--n:4">' in views.project_page(store, "wide", ver="x")
    # nothing at all yet: a placeholder that says how steps arrive
    create(store, "empty", {})
    empty = views.project_page(store, "empty", ver="x")
    assert "No steps yet." in empty and 'class="plane"' not in empty


def test_answers_show_what_was_chosen_and_markdown_is_rendered():
    ans = {"action": "choose", "params": {}, "values": {"value": "Retro NES", "notes": ""}}
    assert views.answer_text(ans) == "Retro NES"
    assert views.answer_text({"action": "answer", "text": "yes"}) == "yes"
    assert views.answer_text("not an answer") is None
    assert views._value(ans) == '<span class="v">Retro NES</span>'
    assert '<div class="v long md"><h2>Recheck</h2>' in views._value("## Recheck\n\nok")
    assert views._value("two\nlines").startswith('<div class="v long text">')


def test_the_standalone_page_is_the_board_and_every_step_in_a_disclosure(store):
    board_project(store)
    page = views.render(store, "v", "html")
    assert "<nav" not in page and "datastar" not in page and 'id="drawer"' not in page
    assert '<div class="node card is-succeeded" id="n-a"' in page and "href=" not in card(page, "a")
    assert page.count('<details class="std"') == 6
    assert "traceback &lt;here&gt;" in page  # the full error, in c's detail


def test_a_steps_detail(store):
    create(store, "v", {
        "make": {"run": "test.add", "in": {"a": src("n"), "b": d(1)}},
        "agent": {"run": "test.open", "doc": "Write <the> thing",
                  "in": {"prompt": d("Do <b>it</b>\nthen stop"), "made": src("make/sum")},
                  "outputs": {"answer": {"type": "string", "doc": "What it found"}}},
    }, inputs={"n": "int"})
    run = store.runs_dir("v") / "r1"
    run.mkdir(parents=True)
    (run / "input.json").write_text('{"prompt": "Do <b>it</b>\\nthen stop", "made": 2}')
    (run / "stderr.log").write_text("step one\n<script>alert(1)</script>\n")
    with store.lock("v"):
        store.write_state("v", {"inputs": {"n": 1}, "steps": {
            "make": {"status": "succeeded", "outputs": {"sum": 2}},
            "agent": {"status": "succeeded", "run_ids": ["r1"],
                      "started": "2026-01-01T10:00:00Z", "finished": "2026-01-01T10:01:30Z",
                      "outputs": {"answer": "<i>42</i>", "ports": {}, "extra": {},
                                  "results": [], "cost_usd": 0.1234567}}}})
    store.append("v", {"kind": "step.status", "step": "agent", "from": "pending",
                       "to": "running"},
                 {"kind": "step.status", "step": "agent", "from": "running", "to": "failed",
                  "error": "exit code 2"},
                 {"kind": "step.status", "step": "agent", "from": "pending", "to": "running"},
                 {"kind": "step.status", "step": "agent", "from": "running",
                  "to": "succeeded"},
                 {"kind": "message", "thread": "step-agent", "from": "agent",
                  "to": "orchestrator", "body": "Which <file>?"},
                 {"kind": "message", "thread": "other", "from": "x", "body": "not here"})
    html = views.step_detail(store, "v", "agent")
    head = html[:html.index("</header>")]
    assert "<h2>agent</h2>" in head and '<p class="d-doc">Write &lt;the&gt; thing</p>' in head
    facts = dict(re.findall(r"<div><dt>([^<]+)</dt><dd>(.*?)</dd></div>", head))
    assert facts["Status"] == "succeeded" and facts["Function"] == "<code>test.open</code>"
    assert facts["Duration"] == "1m 30s" and facts["Cost"] == "$0.12"  # cost as money
    sections = re.findall(r'<h3 class="label">([^<]+)</h3>', html)
    assert sections == ["Messages", "Outputs", "Prompt", "Inputs", "Log output", "Attempts"]
    # a named value: its name (type on demand, and in the name's title), its doc, its value
    assert ('<span class="f-name" title="string">answer</span><span class="f-type">string'
            '</span></div><p class="f-doc">What it found</p><div class="f-v"><div class="v '
            'prose"><p>&lt;i&gt;42&lt;/i&gt;</p></div></div>') in html
    assert '<button type="button" class="types-toggle" aria-pressed="false"' in html
    assert "0.123457" not in html  # cost is a fact of the run, in the header, not an output
    assert '<div class="prompt">Do &lt;b&gt;it&lt;/b&gt;\nthen stop</div>' in html
    # an input says where it comes from, as a link; a value set in the plan says nothing
    assert ('<span class="f-name" title="int">made</span><span class="f-type">int</span>'
            '<span class="f-from">← <a href="/projects/v/steps/make" data-step="make">'
            'make/sum</a></span></div><div class="f-v"><code class="v">2</code></div>') in html
    assert "set in the plan" not in html
    assert "&lt;script&gt;alert(1)&lt;/script&gt;" in html and "<script>" not in html
    assert ('<span class="m-from">agent</span><span class="m-to">→ orchestrator</span>' in html
            and "Which &lt;file&gt;?" in html)
    assert "not here" not in html
    runs = html[html.index("Attempts</h3>"):]
    assert runs.index("succeeded") < runs.index("failed")  # newest first
    assert "exit code 2" in runs
    assert "<i>" not in html and "<b>it" not in html


def test_a_running_steps_detail_shows_its_progress_and_what_it_submitted(store):
    create(store, "v", {"agent": {"run": "test.open", "in": {},
                                  "outputs": {"answer": "string"}}})
    run = store.runs_dir("v") / "r1"
    run.mkdir(parents=True)
    (run / "stderr.log").write_text("thinking\n")
    (run / "submitted.json").write_text('{"answer": "so far"}')
    with store.lock("v"):
        store.write_state("v", {"inputs": {}, "steps": {"agent": {
            "status": "running", "run_ids": ["r1"], "started": "2026-01-01T10:00:00Z"}}})
    html = views.step_detail(store, "v", "agent")
    assert '<h3 class="label">Progress</h3></div><pre class="tail">thinking</pre>' in html
    outputs = html[html.index("Outputs submitted so far"):html.index("</section>",
                                                                     html.index("so far"))]
    assert ">answer</span>" in outputs and "so far" in outputs
    assert ">ports</span>" not in outputs and ">results</span>" not in outputs  # the fn's own
    (run / "submitted.json").unlink()
    html = views.step_detail(store, "v", "agent")
    assert "None yet. It hands on: answer." in html and "ports" not in html


# ---- what needs a person ------------------------------------------------------------------


def test_long_descriptions_fold_and_inputs_show_their_docs(store):
    create(store, "v", {"a": {"run": "test.add", "in": {"a": src("who"), "b": src("k")}}},
           inputs={"who": {"type": "int", "doc": "Who <b>counts</b>"}, "k": "int"})
    store.update_project("v", "A long description. " * 12)
    store.set_input("v", "k", 3, "test", "")
    page = views.project_page(store, "v", ver="x")
    assert '<details class="about" data-preserve-attr="open"><summary><span class="clamp">' \
        "A long description." in page
    # plan inputs sit under the board: name, value (an unset one marked), doc
    assert ('<dt>who</dt><dd><span class="attn">not set</span><p class="meta">Who &lt;b&gt;'
            'counts&lt;/b&gt;</p></dd>') in page
    assert '<dt>k</dt><dd><code class="v">3</code></dd>' in page
    assert page.index('id="graph"') < page.index('<h2 class="label">Inputs</h2>')


def test_messages_are_threads_with_notes_and_open_questions_marked(store):
    create(store, "v", {"c": {"run": "test.boom", "in": {}, "doc": "Break <it>"},
                        "a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}}})
    store.append("v",
                 {"kind": "message", "thread": "step-c", "from": "c", "to": "orchestrator",
                  "body": "Which **DB**?", "needs_reply": True},
                 {"kind": "message", "thread": "step-a", "from": "a", "to": "orchestrator",
                  "body": "Moving the helpers\nrather than deleting them", "needs_reply": False},
                 {"kind": "message", "thread": "step-a", "from": "a", "to": "orchestrator",
                  "body": "Keep the old names?"},  # no needs_reply: a question
                 {"kind": "message", "thread": "step-a", "from": "orchestrator", "to": "a",
                  "body": "No shims."})
    board = views.load_board(store, "v")
    panel = views.messages_panel(store, board)
    # threads, latest first; one waiting on a reply opens, an answered one stays folded
    assert panel.index('id="th-step-a"') < panel.index('id="th-step-c"')
    assert 'id="th-step-c" data-preserve-attr="open" open>' in panel
    assert 'id="th-step-a" data-preserve-attr="open">' in panel
    assert '<span class="m-tag await">1 awaiting reply</span>' in panel
    assert '<span class="th-doc">Break &lt;it&gt;</span>' in panel
    assert "<strong>DB</strong>" in panel  # markdown bodies render
    assert "Moving the helpers<br>rather than deleting them" in panel
    assert '<span class="m-tag">note</span>' in panel
    assert '<li class="m m-lead">' in panel and '<li class="m m-step">' in panel
    page = views.project_page(store, "v", ver="x")  # questions for the orchestrator are not
    assert "Needs you" not in page and '<div id="messages">' in page  # a person's to answer
    detail = views.step_detail(store, "v", "a")  # its conversation comes before its inputs
    assert detail.index(">Messages</h3>") < detail.index(">Inputs</h3>")
    assert "Awaiting reply" in views.step_detail(store, "v", "c")


# ---- the index ----------------------------------------------------------------------------


def test_the_project_index(store):
    create(store, "v", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)},
                              "doc": "Add <them>"},
                        "b": {"run": "test.boom", "in": {}},
                        "c": {"run": "test.add", "in": {"a": d(1), "b": d(1)}}})
    store.create_project("w", "second")
    with store.lock("v"):
        store.write_state("v", {"inputs": {}, "steps": {
            "a": {"status": "running", "started": "2026-01-01T10:00:00Z"},
            "b": {"status": "failed"}, "c": {"status": "succeeded"}}})
    page = views.index(store, ver="x")
    assert '<a href="/projects/v">v</a>' in page and '<a href="/projects/w">w</a>' in page
    assert '<p class="about">the v project</p>' in page and "second" in page
    assert '<span class="bar" role="img" aria-label="1 succeeded, 1 running, 1 failed">' in page
    assert '<span class="meta">1 of 3</span>' in page
    assert '<a href="/projects/v#step:a">' in page and "Add &lt;them&gt;" in page
    assert "No steps yet." in page  # w
    assert "Needs you" not in page  # what asks for a person is the inbox alone
    assert re.search(r'<time datetime="\d{4}-\d\d-\d\dT[\d:]+Z"', page)
    assert '<a href="/fns">Functions</a>' in page and '<a href="/" aria-current="page">' in page



def test_one_nav_whose_switcher_names_the_project_and_whose_sections_follow_it(store):
    create(store, "v", {"a": {"run": "core.echo", "in": {"value": d(1)}, "doc": "A"}})
    create(store, "w", {"a": {"run": "core.echo", "in": {"value": d(1)}}})
    store.update_project("w", archived=True)

    def nav(page):
        return re.findall(r'<nav class="top".*?</nav>', page, re.DOTALL)

    def button(page):
        return re.search(r'<summary[^>]*><span class="sw-name">([^<]*)</span>', page)[1]

    def links(page):  # the sections, and which is marked
        inner = re.search(r'<span class="links">(.*?)</span>', page)[1]
        return re.findall(r'<a href="[^"]*"(?: aria-current="(\w+)")?>(\w+)</a>', inner)

    board = views.project_page(store, "v", ver="x")
    assert len(nav(board)) == 1 and 'class="ptabs"' not in board
    assert button(board) == "v" and '<h1 class="vh">v</h1>' in board
    assert links(board) == [("page", "Plan"), ("", "Log"), ("", "History"), ("", "Functions")]
    menu = re.search(r'<div class="menu">(.*?)</div></details>', board)[1]
    assert menu.index('href="/projects/v" aria-current="page"') < menu.index("Archived") \
        < menu.index('href="/projects/w"')  # archived projects come last
    history = views.LogQuery.parse({"kind": list(L.HISTORY_KINDS)})
    marked = {name: cur for cur, name in links(views.log_page(store, "v", history)) if cur}
    assert marked == {"History": "page"}
    assert ("page", "Log") in links(views.log_page(store, "v", views.LogQuery()))
    assert ("page", "Functions") in links(views.fns_page(store, "v"))
    step = views.step_page(store, "v", "a", "x")
    assert button(step) == "v" and ("true", "Plan") in links(step)
    home = views.index(store)
    assert button(home) == "All projects"
    assert links(home) == [("page", "Projects"), ("", "Log"), ("", "Functions")]
    inbox = views.inbox_page(store, None, "open", "x")
    assert '<a id="nav-inbox" href="/inbox" aria-current="page">' in inbox
    assert nav(views.project_page(store, "v")) == []  # the standalone plan_view has none
    assert '<div class="phead"><h1>v</h1></div>' in views.project_page(store, "v")


def test_values_are_escaped(store):
    create(store, "v", {"a": {"run": "core.echo", "in": {"value": d("<script>x</script>")},
                              "doc": "<script>doc</script>"},
                        "p": {"run": "test.window", "in": {"seconds": d(0)}}},
           outputs={"out": {"source": "a/value"}})
    store.update_project("v", "<b>bold</b>")
    run = store.runs_dir("v") / "r1"
    run.mkdir(parents=True)
    (run / "stderr.log").write_text("<script>progress</script>\n")
    with store.lock("v"):
        store.write_state("v", {"inputs": {}, "steps": {
            "a": {"status": "succeeded", "outputs": {"value": "<script>alert(2)</script>"}},
            "p": {"status": "running", "run_ids": ["r1"]}}})
    store.append("v", {"kind": "message", "thread": "t", "from": "<i>me</i>", "to": "<u>you</u>",
                       "body": "<script>alert(3)</script>"})
    pages = [views.project_page(store, "v", ver="x"), views.index(store, ver="x"),
             views.log_page(store, "v", views.LogQuery()), views.render(store, "v", "html"),
             views.step_detail(store, "v", "a")]
    for page in pages:
        assert "<script>alert" not in page and "<b>bold" not in page
        assert "<script>x" not in page and "<script>doc" not in page
        assert "<script>progress" not in page and "<i>me" not in page
    assert "&lt;script&gt;progress&lt;/script&gt;" in pages[0]
    assert '<details class="about"' not in pages[0]  # a short description is not folded
    assert 'title="&lt;script&gt;doc&lt;/script&gt;"' in pages[0]  # a's chip
    assert "&lt;script&gt;doc&lt;/script&gt;" in pages[4]  # its detail
    assert "&lt;script&gt;alert(3)&lt;/script&gt;" in pages[0]  # the unanswered message
    assert "&lt;b&gt;bold&lt;/b&gt;" in pages[3]
    log = pages[2]
    assert "t from &lt;i&gt;me&lt;/i&gt; → &lt;u&gt;you&lt;/u&gt;: &lt;script&gt;alert(3)" in log
    assert "&quot;body&quot;: &quot;&lt;script&gt;alert(3)&lt;/script&gt;&quot;" in log


# ---- small parts --------------------------------------------------------------------------


def test_durations_and_log_summaries():
    assert [views.dur(s) for s in (0.4, 7.25, 42, 724, 3900, 90061)] == \
        ["0.4s", "7.2s", "42s", "12m 4s", "1h 5m", "1d 1h 1m"]
    rec = {"kind": "step.submit", "step": "logic", "run": "r1",
           "outputs": {"interface": "x", "branch": "y"}}
    assert views.log_summary(rec) == "logic submitted interface, branch"


def test_types_render_readably():
    assert views.type_text("string[]") == "string[]"
    assert views.type_text(["null", {"type": "enum", "symbols": ["a", "b"]}]) == "enum(a|b)?"
    assert views.type_text({"type": "array", "items": {"type": "record", "fields": {
        "f": "int", "g": "string?"}}}) == "{f: int, g: string?}[]"


def test_the_functions_page_groups_by_scope_and_shows_collisions(store):
    create(store, "v", {})
    write_fn(store.home / "fns", "mine.fn", {"xs": "string[]"},
             {"pick": {"type": "enum", "symbols": ["a", "b"]}}, spec={"doc": "<i>mine</i>"})
    write_fn(store.project_dir("v") / "fns", "test.add", {"a": "int"}, {"sum": "int"})
    write_fn(store.project_dir("v") / "fns", "v.local")
    page = views.fns_page(store, "v")
    sections = {m[0]: m[1] for m in re.findall(r"<h2>([^<]+)</h2>(.*?)(?=<h2>|</main>)", page,
                                               re.DOTALL)}
    assert set(sections) == {"Built-in", "Global", "Project (v)"}
    assert "<b>core.echo</b>" in sections["Built-in"] \
        and "<b>thread.post</b>" in sections["Built-in"]
    assert "<b>mine.fn</b>" in sections["Global"] and "<b>test.add</b>" in sections["Global"]
    assert "xs: <code>string[]</code>" in sections["Global"]
    assert "pick: <code>enum(a|b)</code>" in sections["Global"]
    assert "&lt;i&gt;mine&lt;/i&gt;" in sections["Global"]
    project = sections["Project (v)"]
    assert "<b>v.local</b>" in project
    clash = project[project.index('<div class="fn problem">'):]
    assert "<b>test.add</b>" in clash and "fn test.add collides with the global fn" in clash
    plain = views.fns_page(store)
    assert "Project (" not in plain and "v.local" not in plain
