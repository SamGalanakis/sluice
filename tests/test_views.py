"""The dashboard's views (SPEC §8): the Mermaid text plan_view gives agents, the board of step
cards and its layout, a step's detail, the "Needs you" lines, the index, and escaping."""

import datetime as dt
import html
import json
import os
import re

from sluice import log as L
from sluice import util, views
from tests.conftest import create, d, src, write_fn


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


def lanes(page):
    """The step ids on the board, box by box, each {row: ids}."""
    out = []
    for box in re.findall(r'<li class="box"><ol class="rows"[^>]*>(.*?)</ol></li>', page,
                          re.DOTALL):
        out.append({int(r): re.findall(r'id="n-([^"]+)"', cards) for r, cards in
                    re.findall(r'<li class="row" style="--r:(\d+)">(.*?)</li>', box,
                               re.DOTALL)})
    return out


def board_edges(page):
    """The edges the board draws: {(from, to): names}."""
    data = json.loads(html.unescape(re.search(r'<sluice-board [^>]*edges="([^"]*)"', page)[1]))
    return {(f, t): n for f, t, n in data}


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
    # the glyph's word, for assistive tech, then the id and its time: "succeeded, a, 12m 4s"
    assert '<span class="vh">succeeded, </span>' in a
    assert ('<span class="sid">a<span class="sep">,</span></span><span class="dur">12m 4s'
            '</span>') in a
    # just the name and, small, its time: outputs, engine and cost are in the drawer
    assert "sum" not in a and "test.add" not in a and "$" not in a
    assert "title=" not in card(page, "b").split(">", 1)[0]  # no doc, nothing to say
    assert "is-manual" in card(page, "b")
    c = card(page, "c")  # failed: its error's last line (the exception) is the tooltip
    assert "is-failed" in c and 'title="traceback &lt;here&gt;"' in c
    each = card(page, "each")  # running: its progress is the tooltip, done/total beside it
    assert "is-running" in each and 'title="halfway there"' in each and "1/3" in each
    assert 'data-since="2026-01-01T10:12:05Z"' in each  # its running time stays current
    assert "is-stale" in card(page, "late") and "Its inputs changed" in card(page, "late")
    assert card(page, "fmt").startswith('<a class="node chip is-succeeded"')  # glue: dashed
    # first whether the work moves (counts, the switches), then the board; the plan's result
    # and inputs follow it (plan inputs and outputs are not board nodes)
    summary = page[page.index('<div id="summary">'):page.index('id="graph"')]
    assert "3 of 6 succeeded · 1 running · 1 stale · 1 failed" in summary
    assert '<span class="bar" role="img"' in summary and ">Pause</button>" in summary
    facts = page[page.index('<section id="result" class="plan-facts">'):]
    assert page.index('id="graph"') < page.index('id="result"')
    assert "<dt>total</dt><dd><code class=\"v\">4</code></dd>" in facts
    assert "<dt>n</dt><dd><code class=\"v\">1</code></dd>" in facts
    assert 'data-node="o:' not in page and 'data-node="i:' not in page
    # the page: the drawer that shows a step, and the live stream
    assert '<sluice-drawer data-preserve-attr="data-rocket-host"><div class="scrim"' in page
    assert 'id="drawer"' in page
    assert "'/projects/v/steps/' + encodeURIComponent($step)" in html.unescape(page)
    assert "data-init=\"@get('/projects/v/stream', {retry: 'always'" in page
    assert '<script type="module" src="/static/sluice.js">' in page
    assert '<script type="module" src="/static/datastar-rocket-1.0.4.js">' in page
    assert "mermaid" not in page


def test_each_independent_piece_of_work_is_its_own_box(store):
    # x and w run after a: their lanes share a's box (the after edges join them), in rows by
    # depth from the box's first step, each lane's cards together in a row; z is joined to
    # nothing, so it has a box of its own
    create(store, "v", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
                        "x": {"run": "test.add", "in": {"a": d(2), "b": d(2)}, "after": ["a"]},
                        "w": {"run": "test.add", "in": {"a": d(4), "b": d(4)}, "after": ["a"]},
                        "y": {"run": "test.add", "in": {"a": src("x/sum"), "b": d(1)}},
                        "z": {"run": "test.add", "in": {"a": d(3), "b": d(3)}}})
    page = views.project_page(store, "v", ver="abc")
    assert '<ol class="boxes boxed">' in page
    assert lanes(page) == [{1: ["a"], 2: ["x", "w"], 3: ["y"]}, {1: ["z"]}]
    assert re.search(r'class="node [^"]*lane-start[^"]*"[^>]* id="n-w"', page)  # w's lane begins
    assert not re.search(r'class="node [^"]*lane-start[^"]*"[^>]* id="n-x"', page)
    # a phone stacks the box lane by lane: each card's --o is its lane, then its depth
    assert re.search(r'style="--o:1001"[^>]* id="n-x"', page)  # x: the box's lane 1, row 1
    assert re.search(r'style="--o:2001"[^>]* id="n-w"', page)  # w: lane 2, row 1
    assert board_edges(page)[("s:a", "s:x")] == "after"
    create(store, "w", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}}})
    assert '<ol class="boxes">' in views.project_page(store, "w", ver="abc")  # one: no box


def test_the_board_lays_steps_out_in_lanes_of_rows_by_dependency_depth(store):
    board_project(store)
    page = views.project_page(store, "v", ver="abc")
    # the steps joined by handoffs make one lane; c hands nothing on, so it stands apart
    assert lanes(page) == [{1: ["a"], 2: ["fmt", "b", "each"], 3: ["late"]}, {1: ["c"]}]
    assert '<ol class="rows" style="--rows:3">' in page  # a box has the rows it uses
    # a and c are joined by nothing: each is its own box
    assert '<ol class="boxes boxed">' in page
    # the edges, one per handoff, named by their ports, for <sluice-board> to draw
    assert board_edges(page) == {
        ("s:a", "s:fmt"): "sum → values", ("s:a", "s:b"): "sum → a",
        ("s:a", "s:each"): "sum → tag", ("s:a", "s:late"): "sum → a",
        ("s:b", "s:late"): "sum → b"}
    assert '<svg class="edges" aria-hidden="true" data-ignore-morph>' in page  # drawn, kept
    assert "hands on a value" in page and "runs after" not in page  # the legend
    # an `after` orders lanes without joining them by a handoff; they share a box
    create(store, "two", {
        "a1": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
        "a2": {"run": "test.add", "in": {"a": src("a1/sum"), "b": d(1)}},
        "b1": {"run": "test.add", "in": {"a": d(1), "b": d(1)}, "after": ["a2"]},
        "b2": {"run": "test.add", "in": {"a": src("b1/sum"), "b": d(1)}}})
    two = views.project_page(store, "two", ver="x")
    assert lanes(two) == [{1: ["a1"], 2: ["a2"], 3: ["b1"], 4: ["b2"]}]
    assert board_edges(two)[("s:a2", "s:b1")] == "after" and "runs after" in two
    # inside a lane, a row follows the row above it: crossings undone
    create(store, "cross", {
        "l": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
        "r": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
        "under_r": {"run": "test.add", "in": {"a": src("r/sum"), "b": d(1)}},
        "under_l": {"run": "test.add", "in": {"a": src("l/sum"), "b": d(1)}},
        "join": {"run": "test.add", "in": {"a": src("under_l/sum"), "b": src("under_r/sum")}}})
    assert lanes(views.project_page(store, "cross", ver="x")) == [
        {1: ["l", "r"], 2: ["under_l", "under_r"], 3: ["join"]}]
    # nothing at all yet: a placeholder that says how steps arrive
    create(store, "empty", {})
    empty = views.project_page(store, "empty", ver="x")
    assert "No steps yet." in empty and 'class="plane"' not in empty


def test_a_pending_step_says_what_it_waits_on_and_the_next_ones_stand_out(store):
    create(store, "v", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
                        "b": {"run": "test.add", "in": {"a": src("a/sum"), "b": d(1)}},
                        "c": {"run": "test.add", "in": {"a": src("b/sum"), "b": d(1)}}})
    with store.lock("v"):
        store.write_state("v", {"inputs": {}, "steps": {
            "a": {"status": "running", "started": "2026-01-01T10:00:00Z"}}})
    page = views.project_page(store, "v", ver="x")
    assert 'class="node card is-pending is-next" id="n-b"' in page  # starts once a finishes
    assert 'class="node card is-pending" id="n-c"' in page  # further off
    assert 'title="waits on b (pending)"' in card(page, "c")
    head = views.step_detail(store, "v", "b").split("</header>")[0]
    # a row of its own, each step led by its status glyph
    assert re.search(r'<div class="wide"><dt>Waits on</dt><dd><span class="dep"><span class="g '
                     r'g-running".*?<span class="vh">running, </span></span><a href="/projects/v/'
                     r'steps/a" data-step="a">a</a></span></dd></div>', head)
    assert "Waits on" not in views.step_detail(store, "v", "a")


def test_a_missing_or_stale_runner_beat_says_so_on_the_index_and_project(store):
    create(store, "v", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}}})
    # no runner.json says nothing: its absence is not evidence the runner is down
    for page in (views.index(store), views.project_page(store, "v", "x")):
        assert "Runner stopped" not in page
    assert views.runner_state(store.home) == "none"
    beat = dt.datetime.now(dt.UTC).strftime("%Y-%m-%dT%H:%M:%SZ")
    util.atomic_write_json(store.home / "runner.json",
                           {"pid": 1, "started": beat, "beat": beat})
    for page in (views.index(store), views.project_page(store, "v", "x")):
        assert "No runner is running" not in page and "Runner stopped" not in page
    assert views.runner_state(store.home) == "live"
    util.atomic_write_json(store.home / "runner.json",
                           {"pid": 1, "started": beat, "beat": "2026-01-01T10:00:00Z"})
    assert views.runner_state(store.home) == "stale"
    for page in (views.index(store), views.project_page(store, "v", "x")):
        assert ('<p class="attn">Runner stopped · last seen '
                '<time datetime="2026-01-01T10:00:00Z"') in page
        assert "ago</time>" in page


def test_answers_show_what_was_chosen_and_markdown_is_rendered():
    ans = {"action": "choose", "params": {}, "values": {"value": "Retro NES", "notes": ""}}
    assert views.answer_text(ans) == "Retro NES"
    assert views.answer_text({"action": "answer", "text": "yes"}) == "yes"
    assert views.answer_text("not an answer") is None
    assert views._value(ans) == '<span class="v">Retro NES</span>'
    # its headings sit under the page's own: its top heading is an h4, the rest follow
    assert '<div class="v long md"><h4>Recheck</h4>' in views._value("## Recheck\n\nok")
    assert "<h4>A</h4>\n<h6>B</h6>" in views.markdown("# A\n### B")
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
    assert '<h2 id="d-title">agent</h2>' in head
    assert '<p class="d-doc">Write &lt;the&gt; thing</p>' in head
    facts = dict(re.findall(r"<div><dt>([^<]+)</dt><dd>(.*?)</dd></div>", head))
    assert facts["Status"] == "succeeded" and facts["Function"] == "<code>test.open</code>"
    assert facts["Duration"] == "1m 30s" and facts["Cost"] == "$0.12"  # cost as money
    sections = re.findall(r'<h3 class="label">([^<]+)</h3>', html)
    assert sections == ["Outputs", "Prompt", "Inputs", "Log output", "Attempts"]
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
    # its conversation is on the Threads tab: the head links to it; the step has finished, so
    # its unanswered question no longer waits on anyone
    assert ('<a class="d-thread" href="/projects/v/threads#th-step-agent">Thread · 1 message'
            '</a>') in head
    assert "Which &lt;file&gt;?" not in html and "not here" not in html
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
    progress = html[html.index('Progress</h3>'):html.index("</section>")]
    assert '<pre class="tail">thinking</pre>' in progress
    assert 'data-quiet-line' in progress and ' hidden>' in progress \
        and "Quiet for" not in progress  # still writing: the quiet line stays hidden
    outputs = html[html.index("Outputs submitted so far"):html.index("</section>",
                                                                     html.index("so far"))]
    assert ">answer</span>" in outputs and "so far" in outputs
    assert ">ports</span>" not in outputs and ">results</span>" not in outputs  # the fn's own
    (run / "submitted.json").unlink()
    html = views.step_detail(store, "v", "agent")
    assert "None yet. It hands on: answer." in html and "ports" not in html


def _ago(path, minutes):
    old = (dt.datetime.now(dt.UTC) - dt.timedelta(minutes=minutes)).timestamp()
    os.utime(path, (old, old))


def test_a_running_step_gone_quiet_says_so_on_its_card_and_in_its_drawer(store):
    create(store, "v", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
                        "each": {"run": "test.window", "scatter": "tag",
                                 "in": {"seconds": d(0), "tag": src(["a/sum"])}}})
    run = store.runs_dir("v") / "r1"
    run.mkdir(parents=True)
    (run / "stderr.log").write_text("halfway there\n")
    done = store.runs_dir("v") / "r2"  # a scattered step's finished run does not count
    done.mkdir()
    (done / "stderr.log").write_text("finished\n")
    (done / "exit.json").write_text('{"code": 0}')
    live = store.runs_dir("v") / "r3"
    live.mkdir()
    (live / "stderr.log").write_text("working\n")
    _ago(done / "stderr.log", 60)
    with store.lock("v"):
        store.write_state("v", {"inputs": {}, "steps": {
            "a": {"status": "running", "run_ids": ["r1"],
                  "started": "2026-01-01T10:00:00Z"},
            "each": {"status": "running", "run_ids": ["r2", "r3"], "done": 1, "total": 2,
                     "started": "2026-01-01T10:00:00Z"}}})
    # still writing: no quiet mark on the card, none in the drawer
    page = views.project_page(store, "v", ver="x")
    assert "quiet 5m" not in card(page, "a") and "Quiet for" not in card(page, "a")
    assert 'title="halfway there"' in card(page, "a")
    assert "Quiet for" not in views.step_detail(store, "v", "a")
    # its stderr quiet 20 minutes: the card says so small, the tooltip and drawer say so
    _ago(run / "stderr.log", 20)
    page = views.project_page(store, "v", ver="x")
    a = card(page, "a")
    assert ' · quiet 20m' in a and 'class="quiet" data-quiet=' in a
    assert 'title="Quiet for 20m. Last output: halfway there"' in a
    detail = views.step_detail(store, "v", "a")
    assert "Quiet for 20m." in detail and "Last output: halfway there" in detail
    _ago(run / "stderr.log", 65)
    assert "quiet 1h 5m" in card(views.project_page(store, "v", ver="x"), "a")
    # no stderr.log: the run dir's own mtime is the sign of life
    (run / "stderr.log").unlink()
    _ago(run, 20)
    assert "quiet 20m" in card(views.project_page(store, "v", ver="x"), "a")
    detail = views.step_detail(store, "v", "a")
    assert "Quiet for 20m." in detail and "No output yet." in detail
    # a scattered step with one live run writing is not quiet, however old its finished runs
    each = card(views.project_page(store, "v", ver="x"), "each")
    assert "Quiet for" not in each and "quiet 60m" not in each
    _ago(live / "stderr.log", 20)
    assert "quiet 20m" in card(views.project_page(store, "v", ver="x"), "each")


def test_the_log_hides_thread_post_calls_behind_their_message(store):
    create(store, "v", {})
    store.append("v",
                 {"kind": "call", "call": "c1", "fn": "thread.post", "status": "running",
                  "direct": True},
                 {"kind": "message", "thread": "step-a", "from": "a", "body": "the question"},
                 {"kind": "call", "call": "c1", "fn": "thread.post", "status": "succeeded"},
                 {"kind": "call", "call": "c2", "fn": "thread.post", "status": "failed",
                  "error": "nope"},
                 {"kind": "call", "call": "c3", "fn": "test.add", "status": "succeeded"})
    page = views.log_view(store, "v", views.LogQuery())[0]
    assert "the question" in page
    assert "c1 thread.post" not in page  # both of its call rows hide behind the message
    assert "c2 thread.post" in page  # a failed thread.post call still shows
    assert "c3 test.add" in page
    calls = views.log_view(store, "v", views.LogQuery.parse({"kind": ["call"]}))[0]
    assert "c1 thread.post" in calls and "running" in calls and "succeeded" in calls
    assert "the question" not in calls
    # the filter stays a view concern: log_read lists everything
    fns = [r.get("fn") for r in L.read(store.log_dir("v"), kinds=["call"])["records"]]
    assert fns.count("thread.post") == 3


def test_the_kind_filter_renders_one_line_per_group_in_kind_options_order(store):
    create(store, "v", {})
    page = views.log_page(store, "v", views.LogQuery.parse({"kind": ["step"]}))
    values = re.findall(r'name="kind" value="([^"]+)"', page)
    assert values == list(views.KIND_OPTIONS)  # every kind once, in the signal's order
    lines = re.findall(r'<span class="kline">(.*?)</span>', page)
    step = next(l for l in lines if 'value="step"' in l)
    assert step.startswith('<label class="kg"><input type="checkbox" name="kind" '
                           'value="step" data-bind:kinds checked> step</label>')
    for short in ("output", "retry", "status", "submit"):
        assert f"> {short}</label>" in step
    rest = next(l for l in lines if 'value="call"' in l)
    assert 'value="message"' in rest and 'kg' not in rest


# ---- what needs a person ------------------------------------------------------------------


def test_long_descriptions_fold_and_inputs_show_their_docs(store):
    create(store, "v", {"a": {"run": "test.add", "in": {"a": src("who"), "b": src("k")}}},
           inputs={"who": {"type": "int", "doc": "Who <b>counts</b>"}, "k": "int"})
    store.update_project("v", "A long description. " * 12)
    store.set_input("v", "k", 3, "test", "")
    page = views.project_page(store, "v", ver="x")
    assert ('<details class="about" data-preserve-attr="open"><summary><div class="clip md" '
            'style="--lines:3"><p>A long description.') in page
    # markdown renders, and a list of projects shows its opening only
    assert views.first_paragraph("Lash: the workspace.\nHow work is done:\n- rules\n- more") \
        == "Lash: the workspace. How work is done:"
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
                  "body": "No shims."},
                 {"kind": "message", "thread": "step-gone", "from": "gone", "to": "orchestrator",
                  "body": "Still there?"})
    board = views.load_board(store, "v")
    panel = views.threads_panel(store, board)
    # threads, latest first; one waiting on a reply opens, an answered one stays folded
    assert panel.index('id="th-step-a"') < panel.index('id="th-step-c"')
    assert 'id="th-step-c" data-preserve-attr="open" open>' in panel
    assert 'id="th-step-a" data-preserve-attr="open">' in panel
    assert ('<sluice-thread project="v" thread="step-a" last="6" '
            'data-preserve-attr="class data-rocket-host">') in panel
    assert '<span class="th-new" data-ignore-morph></span>' in panel  # the component's count
    assert '<span class="m-tag await">1 awaiting reply</span>' in panel
    assert panel.count("awaiting reply") == 1  # a step that left the plan waits on nothing
    assert ('<span class="th-name">gone</span><span class="th-doc">no longer in the plan'
            '</span>') in panel
    assert '<span class="th-doc">Break &lt;it&gt;</span>' in panel
    assert "<strong>DB</strong>" in panel  # markdown bodies render
    assert "Moving the helpers<br>rather than deleting them" in panel
    assert '<span class="m-tag">note</span>' in panel
    assert '<li class="m m-lead" data-seq="6">' in panel and 'class="m m-step"' in panel
    assert '<a href="/projects/v#step:a">Open a on the plan</a>' in panel
    # a long thread folds all but its last three messages, from its first open question on
    for i in range(5):
        store.append("v", {"kind": "message", "thread": "long", "from": "x", "body": f"m{i}",
                           "needs_reply": False})
    store.append("v", {"kind": "message", "thread": "ask", "from": "x", "to": "orchestrator",
                       "body": "q?"})
    for i in range(4):
        store.append("v", {"kind": "message", "thread": "ask", "from": "x", "body": f"n{i}",
                           "needs_reply": False})
    panel = views.threads_panel(store, views.load_board(store, "v"))
    long = panel[panel.index('id="th-long"'):panel.index("</sluice-thread>",
                                                          panel.index('id="th-long"'))]
    assert "<summary>2 earlier messages</summary>" in long
    ask = panel[panel.index('id="th-ask"'):panel.index("</sluice-thread>",
                                                        panel.index('id="th-ask"'))]
    assert "earlier" not in ask  # its open question stays in view
    # the plan page has no messages; the Threads tab has them, live
    page = views.project_page(store, "v", ver="x")
    assert "Needs you" not in page and "th-step" not in page
    tab = views.threads_page(store, "v", ver="x")
    assert '<a href="/projects/v/threads" aria-current="page">Threads</a>' in tab
    assert "data-init=\"@get('/projects/v/threads/stream'" in tab and 'id="th-step-c"' in tab
    create(store, "quiet", {})
    assert "No messages yet." in views.threads_panel(store, views.load_board(store, "quiet"))


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
    # each row leads with the project's status glyph (its word read first, then the name)
    assert re.search(r'<a href="/projects/v"><span class="g g-running".*?<span class="vh">'
                     r'running, </span></span><span>v</span></a>', page)
    assert re.search(r'<a href="/projects/w"><span class="g g-pending".*?<span>w</span></a>', page)
    assert '<p class="about">the v project</p>' in page and "second" in page
    assert '<span class="bar" role="img" aria-label="1 succeeded, 1 running, 1 failed">' in page
    assert '<span class="meta">1 of 3</span>' in page
    assert '<a href="/projects/v#step:a">' in page and "Add &lt;them&gt;" in page
    assert "No steps yet." in page  # w
    assert "Needs you" not in page  # what asks for a person is the inbox alone
    assert re.search(r'<time datetime="\d{4}-\d\d-\d\dT[\d:]+Z"', page)
    assert '<a href="/fns">Functions</a>' in page
    assert '<a href="/" class="all" aria-current="page">All projects</a>' in page



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
    assert links(board) == [("page", "Plan"), ("", "Threads"), ("", "Log"), ("", "History"),
                            ("", "Functions")]
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
    assert links(home) == [("", "Log"), ("", "Functions")]  # the switcher says where
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
             views.step_detail(store, "v", "a"), views.threads_page(store, "v", "x")]
    for page in pages:
        assert "<script>alert" not in page and "<b>bold" not in page
        assert "<script>x" not in page and "<script>doc" not in page
        assert "<script>progress" not in page and "<i>me" not in page
    assert "&lt;script&gt;progress&lt;/script&gt;" in pages[0]
    assert '<details class="about"' not in pages[0]  # a short description is not folded
    assert 'title="&lt;script&gt;doc&lt;/script&gt;"' in pages[0]  # a's chip
    assert "&lt;script&gt;doc&lt;/script&gt;" in pages[4]  # its detail
    assert "&lt;script&gt;alert(3)&lt;/script&gt;" in pages[5]  # the message, on Threads
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
    clash = project[project.index('<div class="fn problem" id="fn-'):]
    assert "<b>test.add</b>" in clash and "fn test.add collides with the global fn" in clash
    plain = views.fns_page(store)
    assert "Project (" not in plain and "v.local" not in plain


# ---- what is stuck ------------------------------------------------------------------------


def stuck_project(store, running=False):
    """lint failed; fix reads it and ship reads fix (both blocked); notes reads lint but is
    paused; later is paused on its own; go runs or waits apart."""
    create(store, "v", {
        "lint": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
        "fix": {"run": "test.add", "in": {"a": src("lint/sum"), "b": d(1)}},
        "ship": {"run": "test.add", "in": {"a": src("fix/sum"), "b": d(1)}},
        "notes": {"run": "test.add", "in": {"a": src("lint/sum"), "b": d(1)}, "paused": True},
        "later": {"run": "test.add", "in": {"a": d(1), "b": d(1)}, "paused": "not yet"},
        "go": {"run": "test.add", "in": {"a": d(1), "b": d(1)}}})
    with store.lock("v"):
        store.write_state("v", {"inputs": {}, "steps": {
            "lint": {"status": "failed", "error": "exit code 1\nTraceback (most recent call last):"
                     "\n  File x\nValueError: 3 lint errors\n\n"},
            "go": {"status": "running" if running else "succeeded",
                   "started": "2026-01-01T10:00:00Z"}}})


def test_a_failure_blocks_the_steps_downstream_and_the_page_says_so(store):
    stuck_project(store)
    board = views.load_board(store, "v")
    assert board.held == {"fix": ["lint"], "ship": ["lint"], "notes": ["lint"]}
    assert board.stuck == {"blocked": 2, "paused": 2}  # notes counts as paused, not blocked
    page = views.project_page(store, "v", ver="x")
    summary = page[page.index('<div id="summary">'):page.index('id="graph"')]
    assert "1 of 6 succeeded · 1 failed · 2 blocked · 2 paused" in summary
    assert 'aria-label="1 succeeded, 1 failed, 2 blocked, 2 paused"' in summary
    # the attention line leads the page, the failed step a link that opens its drawer
    assert summary.index('class="stuck"') < summary.index('class="sumline"')
    assert ('Stopped: <a href="/projects/v/steps/lint" data-step="lint">lint</a> failed, '
            "blocking 2 steps · 2 paused") in summary
    # blocked cards say so, not in red; a paused one keeps its own look
    fix = card(page, "fix")
    assert 'class="node card is-pending is-blocked"' in fix and ">blocked</span>" in fix
    assert "is-blocked" not in card(page, "notes") and "is-paused" in card(page, "notes")
    assert "is-blocked" not in card(page, "go")
    assert 'title="waits on lint (failed)"' in fix
    # the tab title leads with it, and the page carries the count for the live title
    assert "<title>1 failed · v · sluice</title>" in page
    assert '<span hidden data-title-failed="1"></span>' in page
    # the index row: the project's glyph, then the same line (to the step on the project page)
    index = views.index(store, ver="x")
    assert re.search(r'<a href="/projects/v"><span class="g g-failed"', index)
    assert ('<p class="stuck"><span>Stopped: <a href="/projects/v#step:lint">lint</a> failed, '
            "blocking 2 steps · 2 paused</span></p>") in index
    assert "nothing is running" not in index
    assert "<title>1 failed · Projects · sluice</title>" in index


def test_while_something_runs_the_attention_line_does_not_say_stopped(store):
    stuck_project(store, running=True)
    page = views.project_page(store, "v", ver="x")
    assert "lint</a> failed, blocking 2 steps" in page and "Stopped:" not in page
    create(store, "fine", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}}})
    fine = views.project_page(store, "fine", ver="x")
    assert 'class="stuck"' not in fine and "<title>fine · sluice</title>" in fine


def test_a_failed_steps_drawer_leads_with_its_exception_and_what_it_blocks(store):
    stuck_project(store)
    html_ = views.step_detail(store, "v", "lint")
    head = html_[:html_.index("</header>")]
    # Blocks: every step it holds up, paused ones too, each a link led by its glyph
    blocks = re.search(r'<div class="wide"><dt>Blocks</dt><dd>(.*?)</dd></div>', head)[1]
    assert re.findall(r'data-step="([^"]+)"', blocks) == ["fix", "ship", "notes"]
    assert 'class="g g-paused"' in blocks
    # the error: its last line first, then all of it in a box that starts at its end
    assert ('<p class="err-line">ValueError: 3 lint errors</p><div class="err-box">'
            '<pre class="err">') in html_
    assert views.error_headline("one line") == "one line" and views.error_headline(None) == ""
    # the same line in the card's tooltip and in the log
    assert 'title="ValueError: 3 lint errors"' in card(views.project_page(store, "v", "x"), "lint")
    rec = {"kind": "step.status", "step": "lint", "from": "running", "to": "failed",
           "error": "exit code 1\nValueError: 3 lint errors"}
    assert views.log_summary(rec).endswith(": ValueError: 3 lint errors")
    assert "Blocks" not in views.step_detail(store, "v", "go")


def test_pause_shows_only_where_it_acts(store):
    stuck_project(store, running=True)
    with store.lock("v"):
        state = store.read_state("v")
        state["steps"].update(ship={"status": "stale"}, fix={"status": "skipped",
                                                              "skipped": "no"})
        state["steps"]["done"] = {"status": "succeeded"}
        store.write_state("v", state)

    def switch(sid):
        head = views.step_detail(store, "v", sid).split("</header>")[0]
        m = re.search(r'<button type="submit">(\w+)</button>', head)
        return m[1] if m else None

    assert switch("lint") == "Pause"  # failed: a retry would start it
    assert switch("ship") == "Pause"  # stale: it would re-run
    assert switch("notes") == "Resume" and switch("later") == "Resume"  # paused
    assert switch("go") is None  # running: pausing never stops a running step
    assert switch("fix") is None  # skipped


def test_the_drawer_is_a_labelled_region_with_one_types_switch(store):
    board_project(store)
    page = views.project_page(store, "v", ver="x")
    # the drawer and the live region sit after main (main goes inert behind a phone's sheet)
    tail = page[page.index("</main>"):]
    assert '<aside id="drawer" class="drawer" style="display:none" tabindex="-1" ' \
        'aria-labelledby="d-title"' in tail
    assert '<div id="announce" class="vh" role="status" aria-live="polite"></div>' in tail
    assert page.index('<a class="skip" href="#graph">Skip to plan</a>') < page.index("<nav")
    # the standalone page has no drawer, and its step headings carry no shared id
    assert 'id="d-title"' not in views.render(store, "v", "html")
    detail = views.step_detail(store, "v", "each")
    assert detail.count('class="types-toggle"') == 1


def test_after_and_when_are_links_led_by_their_glyphs(store):
    create(store, "v", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
                        "b": {"run": "test.add", "in": {"a": d(1), "b": d(1)}, "after": ["a"]}})
    head = views.step_detail(store, "v", "b").split("</header>")[0]
    assert re.search(r'<div class="wide"><dt>After</dt><dd><span class="dep"><span class="g '
                     r'g-pending".*?<a href="/projects/v/steps/a" data-step="a">a</a>', head)


def test_a_finished_box_folds_to_one_line(store):
    create(store, "v", {
        "a1": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
        "a2": {"run": "test.add", "in": {"a": src("a1/sum"), "b": d(1)}},
        "a3": {"run": "test.add", "in": {"a": src("a2/sum"), "b": d(1)}},
        "b1": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
        "b2": {"run": "test.add", "in": {"a": src("b1/sum"), "b": d(1)}},
        "c1": {"run": "test.add", "in": {"a": d(1), "b": d(1)}}})
    ok = {"status": "succeeded", "outputs": {"sum": 2}}
    with store.lock("v"):
        store.write_state("v", {"inputs": {}, "steps": {
            "a1": ok, "a2": ok, "a3": {"status": "skipped", "skipped": "no"},
            "b1": ok, "b2": {"status": "running"}, "c1": ok}})
    page = views.project_page(store, "v", ver="x")
    boxes = re.findall(r'<li class="box( done)?">', page)
    assert boxes == [" done", "", ""]  # a's box folds; b's is still running; c is one step
    start = page.index('<li class="box done">')
    folded = page[start:page.index("</details>", start)]
    assert '<details class="fold-box" data-preserve-attr="open" data-box="a1">' in folded
    assert '<span class="sid">a1</span>' in folded
    assert "a3 · 3 steps · 2 succeeded, 1 skipped" in folded
    assert 'id="n-a2"' in folded  # its cards are inside, one click away
    # a plan of one piece of work never folds
    create(store, "w", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
                        "b": {"run": "test.add", "in": {"a": src("a/sum"), "b": d(1)}}})
    with store.lock("w"):
        store.write_state("w", {"inputs": {}, "steps": {"a": ok, "b": ok}})
    assert '<details class="fold-box"' not in views.project_page(store, "w", ver="x")


def test_the_log_reads_a_step_cancel_as_a_sentence(store):
    create(store, "v", {})
    store.append("v", {"kind": "step.cancel", "step": "w", "author": "me",
                       "reason": "too slow"})
    page = views.log_view(store, "v", views.LogQuery())[0]
    assert "w cancelled by me: too slow" in page


def test_the_log_says_what_run_records_mean():
    s = views.log_summary
    assert s({"kind": "step.cancel", "step": "w", "author": "me", "reason": "x"}) == \
        "w cancelled by me: x"
    assert s({"kind": "step.cancel", "step": "w"}) == "w cancelled"
    assert s({"kind": "run.adopt", "step": "a", "run": "r1", "outcome": "watching"}) == \
        "a: run r1 still running; the new runner watches it"
    assert s({"kind": "run.adopt", "call": "c1", "run": "c1", "outcome": "finished"}) == \
        "call c1: run c1 had finished; its result was collected"
    assert "restart" in s({"kind": "run.adopt", "step": "a", "run": "r", "outcome": "restarted"})
    assert s({"kind": "run.orphan", "run": "r9"}) == "run r9 stopped: no step or call claimed it"
    # a step's message on its own thread does not repeat the thread
    assert s({"kind": "message", "thread": "step-a", "from": "a", "to": "orch",
              "body": "hi"}) == "a → orch: hi"
    assert s({"kind": "message", "thread": "t", "from": "a", "body": "hi"}) == "t from a: hi"


def test_the_log_filter_folds_behind_a_summary_that_counts_kinds(store):
    create(store, "v", {})
    page = views.log_page(store, "v", views.LogQuery(kinds=("run", "message")))
    assert '<details class="kinds" open><summary><span>Filter: 2 kinds</span>' in page
    assert "<span>Filter: all kinds</span>" in views.log_page(store, "v", views.LogQuery())
