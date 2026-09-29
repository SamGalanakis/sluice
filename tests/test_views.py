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
    with store.tx():
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
    for box in re.findall(r'<li class="box"(?: id="[^"]*")?><ol class="rows"[^>]*>(.*?)</ol>'
                          r'</li>', page, re.DOTALL):
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
    with store.tx():
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
                        'href="/projects/v/steps/a" data-step="a" aria-description="Add one to n">')
    # the glyph's word, for assistive tech, then the id and its time: "succeeded, a, 12m 4s"
    assert '<span class="vh">succeeded, </span>' in a
    assert ('<span class="sid">a<span class="sep">,</span></span><span class="dur">12m 4s'
            '</span>') in a
    # just the name and, small, its time: outputs, engine and cost are in the drawer
    assert "sum" not in a and "test.add" not in a and "$" not in a
    assert "aria-description=" not in card(page, "b").split(">", 1)[0]  # no doc, nothing to say
    assert "is-manual" in card(page, "b")
    c = card(page, "c")  # failed: its error's last line (the exception) is the tooltip
    assert "is-failed" in c and 'aria-description="traceback &lt;here&gt;"' in c
    each = card(page, "each")  # running: its progress is the tooltip, done/total beside it
    assert "is-running" in each and 'aria-description="halfway there"' in each and "1/3" in each
    assert 'data-since="2026-01-01T10:12:05Z"' in each  # its running time stays current
    assert "is-stale" in card(page, "late") and "Its inputs changed" in card(page, "late")
    assert card(page, "fmt").startswith('<a class="node chip is-succeeded"')  # glue: dashed
    # first whether the work moves (counts, the switches), then the board; the plan's result
    # and inputs follow it (plan inputs and outputs are not board nodes)
    summary = page[page.index('<div id="summary">'):page.index('id="graph"')]
    # the failed one is the stuck sentence's, above: the counts line leaves it out
    assert "3 of 6 succeeded · 1 running · 1 stale · updated" in summary
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


def test_a_lane_that_would_crowd_a_row_starts_below_instead(store):
    # two long lanes side by side after a root; a third hangs from the second by an `after`
    # at a depth where the two still run: three long cards will not fit a row, so the third
    # lane starts below both, its cards together, instead of wrapping in among their rows
    def lane(p, after):
        ids = [f"{p}-{n}-step-of-a-rather-long-lane" for n in ("first", "second", "third")]
        return {ids[0]: {"run": "test.add", "in": {"a": d(1), "b": d(1)}, "after": [after]},
                ids[1]: {"run": "test.add", "in": {"a": src(f"{ids[0]}/sum"), "b": d(1)}},
                ids[2]: {"run": "test.add", "in": {"a": src(f"{ids[1]}/sum"), "b": d(1)}}}
    create(store, "v", {"root": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
                        **lane("left", "root"), **lane("right", "root"),
                        **lane("late", "right-first-step-of-a-rather-long-lane")})
    rows = {r: [sid.split("-step")[0] for sid in ids]
            for r, ids in lanes(views.project_page(store, "v", ver="x"))[0].items()}
    assert rows == {1: ["root"], 2: ["left-first", "right-first"],
                    3: ["left-second", "right-second"], 4: ["left-third", "right-third"],
                    5: ["late-first"], 6: ["late-second"], 7: ["late-third"]}
    # short ids fit a row: the third lane stays beside the others, at its own depth
    create(store, "w", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
                        "b": {"run": "test.add", "in": {"a": src("a/sum"), "b": d(1)}},
                        "c": {"run": "test.add", "in": {"a": d(1), "b": d(1)}, "after": ["a"]}})
    assert lanes(views.project_page(store, "w", ver="x")) == [{1: ["a"], 2: ["b", "c"]}]


def test_the_board_lays_steps_out_in_lanes_of_rows_by_dependency_depth(store):
    board_project(store)
    page = views.project_page(store, "v", ver="abc")
    # the steps joined by handoffs make one lane; c hands nothing on, so it stands apart (and,
    # failed, its box leads: live first)
    assert lanes(page) == [{1: ["c"]}, {1: ["a"], 2: ["fmt", "b", "each"], 3: ["late"]}]
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
    with store.tx():
        store.write_state("v", {"inputs": {}, "steps": {
            "a": {"status": "running", "started": "2026-01-01T10:00:00Z"}}})
    page = views.project_page(store, "v", ver="x")
    assert 'class="node card is-pending is-next" id="n-b"' in page  # starts once a finishes
    assert 'class="node card is-pending" id="n-c"' in page  # further off
    assert 'aria-description="waits on b (pending)"' in card(page, "c")
    head = views.step_detail(store, "v", "b").split("</header>")[0]
    # a row of its own, each step led by its status glyph
    assert re.search(r'<div><dt>Waits on</dt><dd><span class="dep"><span class="g '
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
    with store.tx():
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
    # its state as badges by the title: the status (glyph and word) and how long it ran,
    # when it started and ended in the time's tooltip, then how long ago it ended
    badges = re.search(r'<p class="d-badges">(.*?)</p>', head)[1]
    assert re.search(r'<span class="tag"><span aria-hidden="true"><span class="g g-succeeded"'
                     r'.*?</span>succeeded</span>', badges)
    assert ('<span class="tag" title="started 2026-01-01T10:00:00Z, ended '
            '2026-01-01T10:01:30Z">1m 30s</span>') in badges
    assert '<span class="d-ago">ended <time datetime="2026-01-01T10:01:30Z"' in badges
    assert "<dt>Status</dt>" not in head and "<dt>Duration</dt>" not in head
    # the fn and the cost (as money) are one line of meta under the doc
    assert '<p class="d-meta meta"><code title="function">test.open</code> · $0.12</p>' in head
    sections = re.findall(r'<h3 class="label">([^<]+)</h3>', html)
    assert sections == ["Outputs", "Prompt", "Inputs", "Log output", "Attempts"]
    # a named value is a row of a field list: its name (its type after it on demand, both in
    # its title), then its value beside it and its doc
    assert '<dl class="fields"><div class="f">' in html
    assert ('<div class="f"><dt class="f-k" title="answer: string"><span class="f-name">answer'
            '</span><span class="f-type">string</span></dt><dd class="f-v"><span class="v">'
            '&lt;i&gt;42&lt;/i&gt;</span><p class="f-doc">What it found</p></dd></div>') in html
    assert ('<button type="button" class="types-toggle" aria-pressed="false" '
            'title="Show the types of the values">Types<span class="sw" aria-hidden="true">'
            '</span></button>') in html
    assert "0.123457" not in html  # cost is a fact of the run, in the header, not an output
    assert '<div class="prompt">Do &lt;b&gt;it&lt;/b&gt;\nthen stop</div>' in html
    # an input says where it comes from, a chip linking to the step; a value set in the plan
    # says nothing
    assert ('<dt class="f-k" title="made: int"><span class="f-name">made</span><span '
            'class="f-type">int</span></dt><dd class="f-v"><span class="v num">2</span>'
            '<span class="f-from"><a class="src" href="/projects/v/steps/make" '
            f'data-step="make" title="from make/sum">{views.FROM_ICON}<span class="vh">from '
            '</span><span class="mid"><span class="t">make/sum</span></span></a></span></dd>'
            ) in html
    assert "set in the plan" not in html
    assert "&lt;script&gt;alert(1)&lt;/script&gt;" in html and "<script>" not in html
    # its conversation is on the Threads tab: the head links to it; the step has finished, so
    # its unanswered question no longer waits on anyone
    assert ('<a class="d-thread" href="/projects/v/threads#th-step-agent">Thread · 1 message'
            '</a>') in head
    assert "Which &lt;file&gt;?" not in html and "not here" not in html
    runs = html[html.index("Attempts</h3>"):]
    assert runs.index("Failed") < runs.index("Succeeded")  # oldest first, the current last
    assert "exit code 2" in runs
    assert "<i>" not in html and "<b>it" not in html


def fields_of(html_):
    """Each field's row: name -> (its row's html, whether its value takes the full width)."""
    return {m[2]: (m[0], m[1] == " wide") for m in re.finditer(
        r'<div class="f( wide)?"><dt class="f-k" title="[^"]*"><span class="f-name">([^<]+)'
        r'</span>.*?</dd></div>', html_, re.DOTALL)}


def test_values_read_by_kind_in_a_compact_field_list(store):
    path = "/srv/forks/fork-for-queued-runs-removal"
    create(store, "v", {
        "fork": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
        "agent": {"run": "test.open", "in": {
            "spec": src("fork/sum"), "cwd": src("fork/sum"), "lands": d(True),
            "dry": d(False), "engine": d("opus"), "ticket": d("ABC-3945"), "n": d(3),
            "gone": d(None), "tags": d(["a", "b<c>"]), "note": d("line one\nline two"),
            "blob": d({"k": list(range(40))})}}})
    run = store.runs_dir("v") / "r1"
    run.mkdir(parents=True)
    (run / "input.json").write_text(json.dumps({"cwd": path, "spec": "Do it"}))
    with store.tx():
        store.write_state("v", {"inputs": {}, "steps": {
            "fork": {"status": "succeeded", "outputs": {"sum": 2}},
            "agent": {"status": "succeeded", "run_ids": ["r1"]}}})
    html = views.step_detail(store, "v", "agent")
    f = fields_of(html)
    # a boolean is a small pill, not a raw true
    assert '<dd class="f-v"><span class="v pill pill-true">true</span></dd>' in f["lands"][0]
    assert '<span class="v pill pill-false">false</span>' in f["dry"][0]
    # a plain word is text; a number is a number; null is a quiet none; a short list, commas
    assert '<dd class="f-v"><span class="v">opus</span></dd>' in f["engine"][0]
    assert '<span class="v num">3</span>' in f["n"][0]
    assert '<span class="v quiet">none</span>' in f["gone"][0]
    assert '<ul class="v list"><li>a</li><li>b&lt;c&gt;</li></ul>' in f["tags"][0]
    # an identifier (a ticket, a path) is in the data face, whole in its title, giving way in
    # the middle (its last segment stays), with a copy button named for the field
    assert ('<span class="v id"><code class="mid" title="ABC-3945"><span class="t">ABC-3945'
            '</span></code><button type="button" class="copy" aria-label="Copy ticket" '
            'title="Copy">') in f["ticket"][0]
    assert (f'<code class="mid" title="{path}"><span class="h">/srv/forks'
            '</span><span class="t">/fork-for-queued-runs-removal</span></code>') in f["cwd"][0]
    # where it comes from: a quiet chip on the name's row, after the value, linking the step
    assert re.search(r'</button></span><span class="f-from"><a class="src" '
                     r'href="/projects/v/steps/fork" data-step="fork" title="from fork/sum">',
                     f["cwd"][0])
    # all of these are one row beside their names; multi-line text and a long structure take
    # the full width below theirs
    assert not any(f[n][1] for n in ("lands", "dry", "engine", "n", "gone", "tags", "ticket",
                                     "cwd"))
    assert f["note"][1] and '<div class="v code"><pre>line one\nline two</pre></div>' \
        in f["note"][0]
    assert f["blob"][1] and '<details class="fold code"' in f["blob"][0]
    # the prompt's source is a chip in its section's head, not a line over the prompt
    spec = html[html.index('<h3 class="label">Spec</h3>'):]
    assert spec.startswith('<h3 class="label">Spec</h3><span class="f-from"><a class="src" ')
    assert "</div><div class=\"prompt\">Do it</div>" in spec
    # the type follows the name, and the name's title holds both
    assert ('<dt class="f-k" title="lands: Any"><span class="f-name">lands</span>'
            '<span class="f-type">Any</span></dt>') in f["lands"][0]


def test_a_long_value_takes_the_full_width_below_its_name(store):
    long = "A finding that runs on. " * 12
    create(store, "v", {"agent": {"run": "test.open", "in": {},
                                  "outputs": {"findings": {"type": "string",
                                                           "doc": "What it found"}}}})
    with store.tx():
        store.write_state("v", {"inputs": {}, "steps": {"agent": {
            "status": "succeeded", "outputs": {"findings": long, "ok": True}}}})
    html = views.step_detail(store, "v", "agent")
    f = fields_of(html)
    row, wide = f["findings"]
    # its doc sits on the name's row, the value spans below
    assert wide and ('<dd class="f-v"><div class="f-about"><p class="f-doc">What it found</p>'
                     '</div><div class="v prose"><p>A finding') in row
    assert not f["ok"][1]


def test_attempts_read_oldest_first_each_with_its_start_and_its_whole_error(store):
    create(store, "v", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}}})
    trace = ("exit code 1\nTraceback (most recent call last):\n  File \"x.py\", line 3\n"
             "ValueError: " + "the fork at /tmp/forks/a is gone " * 6 + "<end>")
    with store.tx():
        store.write_state("v", {"inputs": {}, "steps": {"a": {
            "status": "running", "run_ids": ["20260101T100500-a-0-cafe"],
            "started": "2026-01-01T10:05:00Z"}}})
    st = {"kind": "step.status", "step": "a"}
    store.append("v",
                 # the first attempt's running record is gone (trimmed): its start comes
                 # from its run id's stamp
                 {**st, "from": "running", "to": "failed", "at": "2026-01-01T09:01:00Z",
                  "error": "exit code 2", "run_ids": ["20260101T090000-a-0-beef"]},
                 # failed before its start was written, with no run id: says when it ended
                 {**st, "from": "pending", "to": "failed", "at": "2026-01-01T09:30:00Z",
                  "error": "could not start the fn: no claude"},
                 {**st, "from": "failed", "to": "pending", "at": "2026-01-01T09:40:00Z"},
                 {**st, "from": "pending", "to": "running", "at": "2026-01-01T09:40:00Z"},
                 {**st, "from": "running", "to": "failed", "at": "2026-01-01T09:50:30Z",
                  "error": trace},
                 {**st, "from": "failed", "to": "pending", "at": "2026-01-01T10:04:59Z"},
                 {"kind": "run.adopt", "step": "a", "run": "20260101T100500-a-0-cafe",
                  "outcome": "watching", "at": "2026-01-01T10:20:00Z"})
    # the current run's own running record is past the log's end (trimmed or not yet read):
    # the entry supplies it
    page = views.step_detail(store, "v", "a")
    runs = page[page.index("Attempts</h3>"):]
    items = re.findall(r"<li class=\"(a-[a-z]+)[^\"]*\".*?</li>", runs, re.DOTALL)
    assert items == ["a-failed", "a-failed", "a-failed", "a-running"]
    lis = re.findall(r"<li .*?</li>", runs, re.DOTALL)
    assert [re.search(r'<span class="a-n">(\d+)</span>', li)[1] for li in lis] == \
        ["1", "2", "3", "4"]
    # every attempt says when: its start (or, unknown, its end) as a <time> with the exact time
    assert 'started <time datetime="2026-01-01T09:00:00Z"' in lis[0]  # from the run id
    assert "took 1m" in lis[0]
    assert 'ended <time datetime="2026-01-01T09:30:00Z"' in lis[1]
    assert 'started <time datetime="2026-01-01T09:40:00Z"' in lis[2] and "took 10m 30s" in lis[2]
    # the current run's live time says when it started (to the second in its title)
    assert "started" not in lis[3]
    assert '<time title="2026-01-01T10:05:00Z" datetime="2026-01-01T10:05:00Z" data-since=' \
        in lis[3] and "so far" in lis[3]
    assert "kept through a runner restart" in lis[3]
    assert 'class="a-running a-now" aria-current="step"' in lis[3]
    # a failure: its headline, then all of it, whole and escaped, under "Show error"
    assert '<p class="a-err">exit code 2</p>' in lis[0] and "<details" not in lis[0]
    head = views.error_headline(trace)
    assert f'<p class="a-err">{html.escape(head, quote=False)}</p>' in lis[2]
    full = re.search(r'<details class="a-full".*?<pre class="err">(.*?)</pre>', lis[2],
                     re.DOTALL)
    assert full and html.unescape(full[1]) == trace and "<end>" not in lis[2]
    assert "Show error" in lis[2]


def test_a_running_steps_detail_shows_its_progress_and_what_it_submitted(store):
    create(store, "v", {"agent": {"run": "test.open", "in": {},
                                  "outputs": {"answer": "string"}}})
    run = store.runs_dir("v") / "r1"
    run.mkdir(parents=True)
    (run / "stderr.log").write_text("thinking\n")
    with store.tx() as conn:
        store.write_state("v", {"inputs": {}, "steps": {"agent": {
            "status": "running", "run_ids": ["r1"], "started": "2026-01-01T10:00:00Z"}}})
        conn.execute("INSERT INTO submissions (project, run, step, outputs, at) VALUES "
                     """('v', 'r1', 'agent', '{"answer": "so far"}', 'now')""")
    html = views.step_detail(store, "v", "agent")
    progress = html[html.index('Progress</h3>'):html.index("</section>")]
    assert '<pre class="tail">thinking</pre>' in progress
    assert "quiet" not in progress.lower()  # the quiet badge is by the title, not here
    outputs = html[html.index("Outputs so far"):html.index("</section>",
                                                                     html.index("so far"))]
    assert ">answer</span>" in outputs and "so far" in outputs
    assert ">ports</span>" not in outputs and ">results</span>" not in outputs  # the fn's own
    with store.tx() as conn:
        conn.execute("DELETE FROM submissions")
    html = views.step_detail(store, "v", "agent")
    assert "None yet: answer." in html and "ports" not in html


def _ago(path, minutes):
    old = (dt.datetime.now(dt.UTC) - dt.timedelta(minutes=minutes)).timestamp()
    os.utime(path, (old, old))


QUIET_BADGE = r'<span class="tag attn" data-quiet="[^"]+"( hidden)?><span class="vh">, </span>' \
    r'<span class="qt">([^<]*)</span></span>'


def quiet_badge(html_):
    """The text of the one quiet badge in `html_`, '' while it is hidden; None without one."""
    found = re.findall(QUIET_BADGE, html_)
    assert len(found) <= 1
    if not found:
        return None
    hidden, text = found[0]
    assert bool(hidden) == (not text)  # hidden exactly while it says nothing
    return text


def test_a_running_step_gone_quiet_wears_a_quiet_badge_and_nothing_more(store):
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
    with store.tx():
        store.write_state("v", {"inputs": {}, "steps": {
            "a": {"status": "running", "run_ids": ["r1"],
                  "started": "2026-01-01T10:00:00Z"},
            "each": {"status": "running", "run_ids": ["r2", "r3"], "done": 1, "total": 2,
                     "started": "2026-01-01T10:00:00Z"}}})
    # still writing: the card and the drawer's title carry the badge hidden (the ticker shows
    # it once the run goes quiet), and nothing says quiet
    page = views.project_page(store, "v", ver="x")
    assert quiet_badge(card(page, "a")) == ""
    assert 'aria-description="halfway there"' in card(page, "a")
    detail = views.step_detail(store, "v", "a")
    assert quiet_badge(detail) == "" and "quiet" not in re.sub(QUIET_BADGE, "", detail).lower()
    # its stderr quiet 20 minutes: a gold badge "quiet 20m" after the card's time, and the
    # same by the drawer's title; no sentence, and the tooltip is still its last output
    _ago(run / "stderr.log", 20)
    page = views.project_page(store, "v", ver="x")
    a = card(page, "a")
    assert quiet_badge(a) == "quiet 20m" and "·" not in a
    assert a.index('class="dur"') < a.index('class="tag attn"')
    assert 'aria-description="halfway there"' in a and "Quiet for" not in page
    detail = views.step_detail(store, "v", "a")
    head = detail[:detail.index("</header>")]
    assert quiet_badge(re.search(r'<p class="d-badges">(.*?)</p>', head)[1]) == "quiet 20m"
    assert "Quiet for" not in detail and "Last output" not in detail
    assert '<pre class="tail">halfway there</pre>' in detail  # the tail says what it last said
    # to the minute under an hour, then hours and minutes
    _ago(run / "stderr.log", 65)
    assert quiet_badge(card(views.project_page(store, "v", ver="x"), "a")) == "quiet 1h 5m"
    # no stderr.log: the run dir's own mtime is the sign of life
    (run / "stderr.log").unlink()
    _ago(run, 20)
    assert quiet_badge(card(views.project_page(store, "v", ver="x"), "a")) == "quiet 20m"
    detail = views.step_detail(store, "v", "a")
    assert quiet_badge(detail) == "quiet 20m" and "Nothing written yet." in detail
    # a scattered step with one live run writing is not quiet, however old its finished runs
    each = card(views.project_page(store, "v", ver="x"), "each")
    assert quiet_badge(each) == ""
    _ago(live / "stderr.log", 20)
    assert quiet_badge(card(views.project_page(store, "v", ver="x"), "each")) == "quiet 20m"
    # a step that is not running has no badge at all
    with store.tx():
        store.write_state("v", {"inputs": {}, "steps": {"a": {"status": "succeeded",
                                                               "run_ids": ["r1"]}}})
    assert quiet_badge(card(views.project_page(store, "v", ver="x"), "a")) is None
    assert quiet_badge(views.step_detail(store, "v", "a")) is None


def test_the_drawer_puts_the_status_and_the_duration_by_the_title(store):
    create(store, "v", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
                        "b": {"run": "test.add", "in": {"a": src("c/sum"), "b": d(1)}},
                        "c": {"run": "test.add", "in": {"a": d(1), "b": d(1)}}})
    run = store.runs_dir("v") / "r1"
    run.mkdir(parents=True)
    (run / "stderr.log").write_text("working\n")
    started = (dt.datetime.now(dt.UTC) - dt.timedelta(minutes=74)).strftime("%Y-%m-%dT%H:%M:%SZ")
    with store.tx():
        store.write_state("v", {"inputs": {}, "steps": {
            "a": {"status": "running", "run_ids": ["r1"], "started": started},
            "c": {"status": "failed", "error": "boom", "started": "2026-01-01T10:00:00Z",
                  "finished": "2026-01-01T11:14:00Z"}}})

    def badges(sid):
        head = views.step_detail(store, "v", sid)
        head = head[:head.index("</header>")]
        assert re.search(r'<div class="hd"><h2 id="d-title">[^<]+</h2><p class="d-badges">',
                         head)  # beside the title, in its line
        assert "<dt>Status</dt>" not in head and "<dt>Duration</dt>" not in head \
            and "<dt>Started</dt>" not in head  # no longer facts in a grid
        return re.search(r'<p class="d-badges">(.*?)</p>', head)[1]

    # running: its status, then its live time (the start in its tooltip); no "ago"
    a = badges("a")
    assert re.search(r'^<span class="tag"><span aria-hidden="true"><span class="g g-running"'
                     r'.*?</span>running</span><span class="vh">, </span>', a)
    assert re.search(rf'<span class="tag" title="started {started}"><time datetime="{started}"'
                     rf' data-since="{started}">1h 14m</time></span>', a)
    assert "data-ago" not in a and quiet_badge(a) == ""
    # failed: its status, how long it ran, and how long ago it ended
    c = badges("c")
    assert "</span>failed</span>" in c
    assert ('<span class="tag" title="started 2026-01-01T10:00:00Z, ended 2026-01-01T11:14:00Z">'
            "1h 14m</span>") in c
    assert '<span class="d-ago">ended <time datetime="2026-01-01T11:14:00Z"' in c
    # pending (blocked by c): the status alone, as the word the board uses
    b = badges("b")
    assert "</span>blocked</span>" in b and "title=" not in b.replace('title="pending"', "")


def test_the_index_and_the_tab_title_say_a_run_went_quiet(store):
    create(store, "v", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
                        "b": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
                        "c": {"run": "test.add", "in": {"a": d(1), "b": d(1)}}})
    for r in ("r1", "r2"):
        (store.runs_dir("v") / r).mkdir(parents=True)
        (store.runs_dir("v") / r / "stderr.log").write_text("working\n")
    with store.tx():
        store.write_state("v", {"inputs": {}, "steps": {
            "a": {"status": "running", "run_ids": ["r1"], "started": "2026-01-01T10:00:00Z"},
            "b": {"status": "running", "run_ids": ["r2"], "started": "2026-01-01T10:00:00Z"},
            "c": {"status": "failed", "error": "boom"}}})
    # both still writing: the index's running rows carry the badge hidden, nothing says quiet
    index = views.index(store, ver="x")
    now = re.search(r'<ul class="now">(.*?)</ul>', index)[1]
    assert len(re.findall(QUIET_BADGE, now)) == 2 and "quiet" not in re.sub(QUIET_BADGE, "", now)
    assert "<title>1 failed · Projects · sluice</title>" in index
    # a's stderr quiet 40 minutes: its row says so as its card does, and the titles count it
    _ago(store.runs_dir("v") / "r1" / "stderr.log", 40)
    index = views.index(store, ver="x")
    row = re.search(r'<li><a href="/projects/v#step:a">.*?</li>', index)[0]
    assert quiet_badge(row) == "quiet 40m" and "·" not in row
    assert quiet_badge(re.search(r'<li><a href="/projects/v#step:b">.*?</li>', index)[0]) == ""
    assert "<title>1 failed · 1 quiet · Projects · sluice</title>" in index
    page = views.project_page(store, "v", ver="x")
    assert "<title>1 failed · 1 quiet · v · sluice</title>" in page
    # the page carries when each run last wrote, so the title keeps counting as it ages
    mark = re.search(r'<span hidden data-title-failed="1" data-title-quiet="([^"]*)">', page)[1]
    assert len(mark.split()) == 2


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
    fns = [r.get("fn") for r in L.read(store.home, "v", kinds=["call"])["records"]]
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
    assert '<span class="tag attn">1 awaiting reply</span>' in panel
    assert panel.count("awaiting reply") == 1  # a step that left the plan waits on nothing
    assert ('<span class="th-name">gone</span><span class="th-doc">no longer in the plan'
            '</span>') in panel
    assert '<span class="th-doc">Break &lt;it&gt;</span>' in panel
    assert "<strong>DB</strong>" in panel  # markdown bodies render
    assert "Moving the helpers<br>rather than deleting them" in panel
    assert '<span class="tag muted">note</span>' in panel
    assert '<li class="m m-lead" data-seq="6">' in panel and 'class="m m-step"' in panel
    assert '<a href="/projects/v#step:a">Open on the plan</a>' in panel
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
    with store.tx():
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
    # each tab's title names it, then the project, as the Threads tab's does
    assert "<title>History · v · sluice</title>" in views.log_page(store, "v", history)
    assert "<title>Log · v · sluice</title>" in views.log_page(store, "v", views.LogQuery())
    assert "<title>Log · sluice</title>" in views.log_page(store, None, views.LogQuery())
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
    with store.tx():
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
    assert 'aria-description="&lt;script&gt;doc&lt;/script&gt;"' in pages[0]  # a's chip
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
    assert set(sections) == {"Built-in", "Global", "Project"}  # v is the picker's
    assert "<b>core.echo</b>" in sections["Built-in"] \
        and "<b>thread.post</b>" in sections["Built-in"]
    assert "<b>mine.fn</b>" in sections["Global"] and "<b>test.add</b>" in sections["Global"]
    assert "xs: <code>string[]</code>" in sections["Global"]
    assert "pick: <code>enum(a|b)</code>" in sections["Global"]
    assert "&lt;i&gt;mine&lt;/i&gt;" in sections["Global"]
    project = sections["Project"]
    assert "<b>v.local</b>" in project
    clash = project[project.index('<div class="fn problem" id="fn-'):]
    assert "<b>test.add</b>" in clash and "fn test.add collides with the global fn" in clash
    plain = views.fns_page(store)
    assert "<h2>Project</h2>" not in plain and "v.local" not in plain


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
    with store.tx():
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
    # the stuck sentence counts the failed, blocked and paused steps; the counts line does
    # not say them again (the bar's label still counts them all)
    assert re.search(r'<p class="meta sum">1 of 6 succeeded · updated <time', summary)
    assert 'aria-label="1 succeeded, 1 failed, 2 blocked, 2 paused"' in summary
    # the attention line leads the page, the failed step a link that opens its drawer
    assert summary.index('class="stuck"') < summary.index('class="sumline"')
    assert ('Stopped: <a href="/projects/v/steps/lint" data-step="lint">lint</a> failed, '
            "blocking 2 steps · 2 paused") in summary
    # blocked cards say so, not in red; a paused one keeps its own look (the board shows
    # them once it shows every step: they can't run)
    page = views.project_page(store, "v", ver="x", view=views.BoardView(steps="all"))
    fix = card(page, "fix")
    assert 'class="node card is-pending is-blocked"' in fix and ">blocked</span>" in fix
    assert "is-blocked" not in card(page, "notes") and "is-paused" in card(page, "notes")
    assert "is-blocked" not in card(page, "go")
    assert 'aria-description="waits on lint (failed)"' in fix
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


def test_a_failed_steps_drawer_leads_with_its_cause_and_what_it_blocks(store):
    stuck_project(store)
    html_ = views.step_detail(store, "v", "lint")
    head = html_[:html_.index("</header>")]
    # Blocks: every step it holds up, paused ones too, each a link led by its glyph
    blocks = re.search(r'<div><dt>Blocks</dt><dd>(.*?)</dd></div>', head)[1]
    assert re.findall(r'data-step="([^"]+)"', blocks) == ["fix", "ship", "notes"]
    assert 'class="g g-paused"' in blocks
    # the error: its last line first (without the exception's class), then all of it as it
    # was raised, in a box that starts at its end
    assert ('<p class="err-line">3 lint errors</p><div class="err-box">'
            '<pre class="err">exit code 1\nTraceback') in html_
    assert "ValueError: 3 lint errors</pre>" in html_
    assert views.error_headline("one line") == "one line" and views.error_headline(None) == ""
    # the same line in the card's tooltip and in the log
    assert 'aria-description="3 lint errors"' in card(views.project_page(store, "v", "x"), "lint")
    rec = {"kind": "step.status", "step": "lint", "from": "running", "to": "failed",
           "error": "exit code 1\nValueError: 3 lint errors"}
    assert views.log_summary(rec).endswith(": 3 lint errors")
    assert "Blocks" not in views.step_detail(store, "v", "go")


def test_a_failure_headline_is_in_sluices_words(store, monkeypatch):
    monkeypatch.setenv("HOME", "/home/ada")
    h = views.error_headline
    # the exception's class goes, the home directory reads ~, a signal's exit code says so
    raised = ("exit code 1\nTraceback (most recent call last):\n  File x\n"
              "sluice.fn.ShError: /home/ada/.codex/bin/run exited 143: run: stopped at /work/a")
    assert h(raised) == "~/.codex/bin/run exited 143 (terminated: SIGTERM): run: stopped at /work/a"
    assert h("RuntimeError: boom") == "boom" and h("ValueError: 3 lint errors") == "3 lint errors"
    assert h("exit code 137") == "exit code 137 (killed: SIGKILL)"
    assert h("x exited 130") == "x exited 130 (interrupted: SIGINT)"
    assert h("x exited 129") == "x exited 129 (hung up: SIGHUP)"  # 128 + n in general
    assert h("returned non-zero exit status -15.") == \
        "returned non-zero exit status -15 (terminated: SIGTERM)."
    # what is not a class, a home or a signal stays as it was
    assert h("exit code 1") == "exit code 1" and h("exited 300") == "exited 300"
    assert h("Note: /home/adauel/x") == "Note: /home/adauel/x"
    assert h("KeyboardInterrupt") == "KeyboardInterrupt"
    # the card's tooltip and the drawer say it; the drawer keeps the error as raised under it
    create(store, "v", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}}})
    with store.tx():
        store.write_state("v", {"inputs": {}, "steps": {"a": {
            "status": "failed", "error": "RuntimeError: /home/ada/w exited 143"}}})
    said = "~/w exited 143 (terminated: SIGTERM)"
    assert f'aria-description="{said}"' in card(views.project_page(store, "v", ver="x"), "a")
    detail = views.step_detail(store, "v", "a")
    assert (f'<p class="err-line">{said}</p><div class="err-box"><pre class="err">'
            "RuntimeError: /home/ada/w exited 143</pre>") in detail


def test_pause_shows_only_where_it_acts(store):
    stuck_project(store, running=True)
    with store.tx():
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
    assert re.search(r'<div><dt>After</dt><dd><span class="dep"><span class="g '
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
    with store.tx():
        store.write_state("v", {"inputs": {}, "steps": {
            "a1": ok, "a2": ok, "a3": {"status": "skipped", "skipped": "no"},
            "b1": ok, "b2": {"status": "running"}, "c1": ok}})
    page = views.project_page(store, "v", ver="x")
    boxes = re.findall(r'<li class="box( done)?" id="box-(\w+)">', page)
    # a's box folds; b's is still running, so it leads; c is one step
    assert boxes == [("", "b1"), (" done", "a1"), ("", "c1")]
    start = page.index('<li class="box done"')
    folded = page[start:page.index("</details>", start)]
    assert '<details class="fold-box" data-preserve-attr="open" data-box="a1">' in folded
    assert '<span class="sid">a1</span>' in folded
    # its last step, then how many and how they ended: a part of its own, which a phone
    # keeps under the first id while the last one hides
    assert ('<span class="fb-last"><span aria-hidden="true"> … </span><span class="vh"> to '
            '</span>a3<span class="fb-dot"> · </span></span>'
            '<span class="fb-n">3 steps · 2 succeeded, 1 skipped</span>') in folded
    assert 'id="n-a2"' in folded  # its cards are inside, one click away
    # a plan of one piece of work never folds
    create(store, "w", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
                        "b": {"run": "test.add", "in": {"a": src("a/sum"), "b": d(1)}}})
    with store.tx():
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
        "a: run r1 kept through a runner restart"
    assert s({"kind": "run.adopt", "call": "c1", "run": "c1", "outcome": "finished"}) == \
        "call c1: run c1 had finished; its result was collected"
    assert "restart" in s({"kind": "run.adopt", "step": "a", "run": "r", "outcome": "restarted"})
    assert s({"kind": "run.orphan", "run": "r9"}) == "run r9 stopped: no step or call claimed it"
    # a step's message on its own thread does not repeat the thread
    assert s({"kind": "message", "thread": "step-a", "from": "a", "to": "orch",
              "body": "hi"}) == "a → orch: hi"
    assert s({"kind": "message", "thread": "t", "from": "a", "body": "hi"}) == "t from a: hi"


def test_the_log_says_what_project_records_mean(store):
    s = views.log_summary
    assert s({"kind": "project.pause", "paused": True, "author": "dashboard",
              "reason": "deploying"}) == "paused by dashboard: deploying"
    assert s({"kind": "project.pause", "paused": False, "author": "orch-2"}) == \
        "unpaused by orch-2"
    assert s({"kind": "project.archive", "archived": True, "author": "orch"}) == \
        "archived by orch"
    assert s({"kind": "project.archive", "archived": False, "author": "orch",
              "reason": "back"}) == "unarchived by orch: back"
    assert s({"kind": "project.update", "fields": ["description", "icon"],
              "author": "orch"}) == "description and icon changed by orch"
    assert s({"kind": "project.update", "fields": ["icon"], "author": "<b>",
              "reason": "a & b"}) == "icon changed by &lt;b&gt;: a &amp; b"
    # the records update_project writes read the same way
    store.create_project("p", "old")
    store.update_project("p", paused=True, author="dashboard", reason="deploying")
    store.update_project("p", description="new", author="orch")
    recs = L.read(store.home, "p", kinds=["project"])["records"]
    assert [s(r) for r in recs] == ["paused by dashboard: deploying",
                                    "description changed by orch"]


def test_the_log_filter_folds_behind_a_summary_that_counts_kinds(store):
    create(store, "v", {})
    page = views.log_page(store, "v", views.LogQuery(kinds=("run", "message")))
    assert '<details class="kinds" open><summary><span>Filter: 2 kinds</span>' in page
    assert "<span>Filter: all kinds</span>" in views.log_page(store, "v", views.LogQuery())


# ---- the board's order and filters ----------------------------------------------------------


def box_ids(page):
    """The boxes on the board, in order, by their first step."""
    return re.findall(r'<li class="box(?: done)?" id="box-([^"]+)">', page)


def ranked_project(store):
    """Seven independent pieces of work, one of each state, in a plan order that is none of
    their ranks'. `wait` reads the plan input `n`, which has no value; `ask` runs and has an
    open inbox item; `still` runs and has gone quiet; `old` is stale."""
    one = {"run": "test.add", "in": {"a": d(1), "b": d(1)}}
    create(store, "v", {
        "done1": one, "pend1": one, "run1": one, "fail1": one, "pend2": one, "run2": one,
        "old": one, "done2": {**one, "tags": ["ui"]},
        "d3a": one, "d3b": {"run": "test.add", "in": {"a": src("d3a/sum"), "b": d(1)}},
        "wait": {"run": "test.add", "in": {"a": src("n"), "b": d(1)}},
        "ask": one, "still": {**one, "tags": ["ui"]},
    }, inputs={"n": "int"})
    for r in ("r-ask", "r-still"):
        run = store.runs_dir("v") / r
        run.mkdir(parents=True)
        (run / "stderr.log").write_text("working\n")
    _ago(store.runs_dir("v") / "r-still" / "stderr.log", 60)
    ok = {"status": "succeeded", "outputs": {"sum": 2}}
    with store.tx():
        store.write_state("v", {"inputs": {}, "steps": {
            "done1": ok, "done2": ok, "d3a": ok, "d3b": ok, "run1": {"status": "running"},
            "run2": {"status": "running"}, "fail1": {"status": "failed", "error": "boom"},
            "old": {"status": "stale", "outputs": {"sum": 1}},
            "ask": {"status": "running", "run_ids": ["r-ask"]},
            "still": {"status": "running", "run_ids": ["r-still"]}}})
    store.inbox_post("v", "Which one?", sender="ask")


def test_live_first_orders_boxes_by_what_they_need_stably_within_a_rank(store):
    ranked_project(store)
    page = views.project_page(store, "v", ver="x")
    # attention (failed, waiting on a plan input, asking in the inbox, gone quiet), running,
    # pending, stale, done: each rank in the plan's order
    assert box_ids(page) == ["fail1", "wait", "ask", "still", "run1", "run2", "pend1", "pend2",
                             "old", "done1", "done2", "d3a"]
    assert views.RANKS[views.rank(views.load_board(store, "v"), ["d3a", "run1"], frozenset(),
                                  frozenset())] == "running"  # a box takes its most urgent
    # the plan's order, as written
    plan = views.project_page(store, "v", ver="x", view=views.BoardView(order="plan"))
    assert box_ids(plan) == ["done1", "pend1", "run1", "fail1", "pend2", "run2", "old", "done2",
                             "d3a", "wait", "ask", "still"]
    assert 'class="hidden-note"' not in page and 'class="hidden-note"' not in plan


def test_the_toolbar_filters_boxes_and_says_what_it_hides(store):
    ranked_project(store)
    board = views.load_board(store, "v")

    def show(**kw):
        html_ = views.board_html(store, board, True, views.BoardView(**kw))
        note = re.search(r'<p class="hidden-note">(.*?)</p>', html_)
        return box_ids(html_), note and html.unescape(re.sub(r"<[^>]+>", "", note[1])), html_

    ids, note, page = show(show="active")
    assert ids == ["fail1", "wait", "ask", "still", "run1", "run2", "pend1", "pend2", "old"]
    assert note == "3 done boxes hidden · show"
    assert '<a href="/projects/v">show</a>' in page  # back to the clean address
    ids, note, _ = show(show="attention", order="plan")
    assert ids == ["fail1", "wait", "ask", "still"] and note == "8 other boxes hidden · show"
    ids, note, page = show(show="done", order="plan")
    assert ids == ["done1", "done2", "d3a"] and note == "9 unfinished boxes hidden · show"
    assert '<a href="/projects/v?order=plan">show</a>' in page  # the order stays
    # a tag shows the boxes with any step tagged so
    ids, note, _ = show(tag="ui")
    assert ids == ["still", "done2"] and note == "10 boxes not tagged ui hidden · show"
    ids, note, _ = show(tag="ui", show="done")
    assert ids == ["done2"] and note == "11 boxes hidden · show"
    ids, note, page = show(tag="nope")
    assert ids == [] and '<p class="empty">No box matches.</p>' in page
    assert '<option value="nope" selected>nope</option>' in page  # still reads as chosen
    ids, note, page = show(show="attention", tag="ui")
    assert ids == ["still"]
    # the controls: a GET form to the page, each choice with how many boxes it shows (within
    # the tag), the tags the plan uses, and Apply for a page without script
    _, _, page = show(show="active")
    tools = page[page.index('<form class="board-tools"'):page.index("</form>")]
    assert '<form class="board-tools" method="get" action="/projects/v"' in tools
    assert '<input type="radio" name="order" value="live" checked>Live first' in tools
    assert '<input type="radio" name="show" value="active" checked>Active<span class="n">9' \
        in tools
    assert 'value="attention">Attention<span class="n">4</span>' in tools
    assert 'value="done">Done<span class="n">3</span>' in tools
    assert re.search(r'<label class="tag-pick">Tag <select name="tag"><option value="">any'
                     r'</option><option value="ui">ui</option></select></label>', tools)
    # what shows, then the order
    assert tools.index('name="show"') < tools.index('name="tag"') < tools.index('name="order"')
    assert "<noscript><button type=\"submit\">Apply</button></noscript>" in tools
    assert page.index('<form class="board-tools"') < page.index("<sluice-board")


def test_filtering_drops_the_edges_of_hidden_boxes(store):
    create(store, "v", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
                        "b": {"run": "test.add", "in": {"a": src("a/sum"), "b": d(1)}},
                        "c": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
                        "e": {"run": "test.add", "in": {"a": src("c/sum"), "b": d(1)}}})
    ok = {"status": "succeeded", "outputs": {"sum": 2}}
    with store.tx():
        store.write_state("v", {"inputs": {}, "steps": {"a": ok, "b": ok}})
    board = views.load_board(store, "v")
    assert set(board_edges(views.board_html(store, board))) == {("s:a", "s:b"), ("s:c", "s:e")}
    active = views.board_html(store, board, True, views.BoardView(show="active"))
    assert box_ids(active) == ["c"] and set(board_edges(active)) == {("s:c", "s:e")}
    none = views.board_html(store, board, True, views.BoardView(show="attention"))
    assert board_edges(none) == {} and 'class="legend"' not in none
    assert '<p class="empty">Nothing needs attention.</p>' in none


def test_a_board_of_one_box_has_no_toolbar_and_ignores_filters(store):
    create(store, "w", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
                        "b": {"run": "test.add", "in": {"a": src("a/sum"), "b": d(1)}}})
    page = views.project_page(store, "w", ver="x", view=views.BoardView(show="done"))
    assert '<form class="board-tools"' not in page
    assert 'id="n-a"' in page and 'id="n-b"' in page
    # nor does the standalone page (no server to send the form to)
    ranked_project(store)
    assert '<form class="board-tools"' not in views.render(store, "v", "html")


def test_the_board_view_reads_and_writes_its_query():
    v = views.BoardView.parse({"order": ["plan"], "show": ["active"], "tag": ["ui"]})
    assert v == views.BoardView("plan", "active", "ui")
    assert v.query() == "order=plan&show=active&tag=ui"
    assert views.BoardView.parse({"order": ["live"], "show": ["all"], "tag": [""]}).query() == ""
    assert views.BoardView.from_signals({"board": "show=done&tag=x"}) == \
        views.BoardView(show="done", tag="x")
    assert views.BoardView.from_signals({"board": "show=bogus"}) == views.BoardView()
    assert views.BoardView.from_signals({}) == views.BoardView()
    for bad in ({"order": ["sideways"]}, {"show": ["some"]}, {"steps": ["few"]}):
        try:
            views.BoardView.parse(bad)
        except views.BadRequest:
            continue
        raise AssertionError(bad)
    # the steps shown: those that can run by default, `steps=all` for every one
    assert views.BoardView.parse({"steps": ["all"], "show": ["done"]}).query() == \
        "show=done&steps=all"
    assert views.BoardView.parse({"steps": ["runnable"]}).query() == ""
    assert views.BoardView.from_signals({"board": "steps=all"}) == views.BoardView(steps="all")


# ---- the steps that can't run ----------------------------------------------------------------


def unreachable_project(store):
    """Five pieces of work. `f` failed: `f1` and `f2` wait behind it, and `fp`, paused, waits
    behind `f1` with `fp1` after it. `p` is paused (in the plan) with `p1`, stale, reading
    it. `w` reads the plan input `n`, which has no value, and `w1` runs after it. `st` is
    stale (it waits for a retry) with `st1` reading it. `s` was skipped. `ok` succeeded and
    `r` reads it, ready to run."""
    one = {"run": "test.add", "in": {"a": d(1), "b": d(1)}}

    def reads(sid):
        return {"run": "test.add", "in": {"a": src(f"{sid}/sum"), "b": d(1)}}

    create(store, "v", {
        "f": one, "f1": reads("f"), "f2": reads("f1"), "fp": {**reads("f1"), "paused": True},
        "fp1": {**one, "after": ["fp"]},
        "p": {**one, "paused": "not yet"}, "p1": reads("p"),
        "w": {"run": "test.add", "in": {"a": src("n"), "b": d(1)}},
        "w1": {**one, "after": ["w"]},
        "st": one, "st1": reads("st"),
        "s": one, "ok": one, "r": reads("ok")}, inputs={"n": "int"})
    with store.tx():
        store.write_state("v", {"inputs": {}, "steps": {
            "f": {"status": "failed", "error": "boom"},
            "p1": {"status": "stale", "outputs": {"sum": 1}},
            "st": {"status": "stale", "outputs": {"sum": 2}},
            "s": {"status": "skipped", "skipped": "go is false"},
            "ok": {"status": "succeeded", "outputs": {"sum": 2}}}})


def test_a_step_cant_run_behind_a_failure_a_pause_or_a_missing_input(store):
    unreachable_project(store)
    board = views.load_board(store, "v")
    hidden = board.unreachable
    # each cause holds up what is behind it, transitively through handoffs and `after`; a
    # stale step counts as not run; every skipped step is in
    assert hidden == {"f1", "f2", "fp", "fp1", "p1", "w1", "st1", "s"}
    # the frontier stays: what a person acts on (the failed, the paused, the step waiting on
    # an input, the stale one waiting for a retry), and what can run (ready or done)
    assert {"f", "p", "w", "st", "ok", "r"}.isdisjoint(hidden)
    assert [sid for sid in board.blocks if board.halts(sid)] == ["f", "fp", "p", "p1", "w",
                                                                  "st"]
    # a paused step behind a failure is not the frontier: the failure comes first
    assert "fp" in hidden
    # closed downstream: no step left on the board waits on a hidden one
    for sid, b in board.blocks.items():
        if sid not in hidden:
            assert not set(b.waits) & hidden, sid
    # a project's pause does not count
    store.update_project("v", paused=True)
    assert views.load_board(store, "v").unreachable == hidden
    # a healthy board hides nothing: a failure or a missing input with nothing behind it
    create(store, "fine", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
                           "b": {"run": "test.add", "in": {"a": src("a/sum"), "b": d(1)}}})
    assert views.load_board(store, "fine").unreachable == frozenset()
    create(store, "lone", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
                           "b": {"run": "test.add", "in": {"a": src("n"), "b": d(1)}}},
           inputs={"n": "int"})
    with store.tx():
        store.write_state("lone", {"inputs": {}, "steps": {"a": {"status": "failed"}}})
    assert views.load_board(store, "lone").unreachable == frozenset()


def test_the_board_hides_the_steps_that_cant_run_and_says_so(store):
    unreachable_project(store)
    board = views.load_board(store, "v")

    def show(**kw):
        html_ = views.board_html(store, board, True, views.BoardView(**kw))
        note = re.search(r'<p class="hidden-note">(.*?)</p>', html_)
        cards = re.findall(r' id="n-([^"]+)" data-node=', html_)
        return set(cards), note and html.unescape(re.sub(r"<[^>]+>", "", note[1])), html_

    cards, note, page = show()
    assert cards == {"f", "p", "w", "st", "ok", "r"}
    assert note == "8 steps that can't run hidden · show"
    assert '<a href="/projects/v?steps=all">show</a>' in page
    # a box left with no step goes, like a filtered one (its step is counted above)
    assert box_ids(page) == ["f", "w", "p", "st", "ok"] and 'id="box-s"' not in page
    # nothing dangles: every edge joins two cards on the board
    assert set(board_edges(page)) == {("s:ok", "s:r")}
    # the frontier says what waits behind it, quietly in its small line
    assert "+4 behind" in card(page, "f") and "+1 behind" in card(page, "p")
    assert "+1 behind" in card(page, "w") and "+1 behind" in card(page, "st")
    assert "behind" not in card(page, "ok") and "behind" not in card(page, "r")
    # the control: which steps show, after which boxes and before the order
    tools = page[page.index('<form class="board-tools"'):page.index("</form>")]
    assert ('<fieldset class="seg"><legend class="vh">Steps</legend><label><input '
            'type="radio" name="steps" value="runnable" checked>Runnable</label><label><input '
            'type="radio" name="steps" value="all">All steps</label></fieldset>') in tools
    assert tools.index('name="show"') < tools.index('name="steps"') < tools.index('name="order"')
    # the box counts leave out the box the steps filter empties
    assert 'value="done">Done<span class="n">0</span>' in tools
    # every step, laid out as before: the cards, edges and no cue
    cards, note, page = show(steps="all")
    assert cards == set(board.blocks) and note is None
    assert 'name="steps" value="all" checked' in page and "behind" not in page
    assert ("s:f", "s:f1") in board_edges(page) and box_ids(page)[-1] == "s"
    # with a box filter too, one line says both, its link showing everything
    cards, note, page = show(show="active")
    assert note == "1 done box and 7 steps that can't run hidden · show"
    assert '<a href="/projects/v?steps=all">show</a>' in page
    cards, note, page = show(show="active", order="plan")
    assert '<a href="/projects/v?order=plan&amp;steps=all">show</a>' in page
    # the standalone page shows every step (it has no toolbar to show them with)
    assert 'id="n-f1"' in views.render(store, "v", "html")


def test_a_board_of_one_box_offers_only_the_steps_filter(store):
    create(store, "w", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
                        "b": {"run": "test.add", "in": {"a": src("a/sum"), "b": d(1)}},
                        "c": {"run": "test.add", "in": {"a": src("b/sum"), "b": d(1)}}})
    with store.tx():
        store.write_state("w", {"inputs": {}, "steps": {"a": {"status": "failed",
                                                              "error": "boom"}}})
    page = views.project_page(store, "w", ver="x", view=views.BoardView(show="done"))
    start = page.index('<form class="board-tools"')
    tools = page[start:page.index("</form>", start)]
    assert 'name="steps"' in tools and 'name="show"' not in tools and 'name="order"' not in tools
    assert "2 steps that can't run hidden" in tools
    assert 'id="n-a"' in page and 'id="n-b"' not in page and "+2 behind" in card(page, "a")
    # the stuck sentence and the bar count every step still
    assert "blocking 2 steps" in page and 'aria-label="1 failed, 2 blocked"' in page
    # a board where every step is hidden says so
    create(store, "x", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}}})
    with store.tx():
        store.write_state("x", {"inputs": {}, "steps": {"a": {"status": "skipped",
                                                              "skipped": "no"}}})
    assert '<p class="empty">No step can run.</p>' in views.project_page(store, "x", ver="x")
