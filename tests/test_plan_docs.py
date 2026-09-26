"""Optional docs on plan inputs (`{"type": T, "doc": "..."}`) and steps (`doc`) (SPEC §5): how
they validate, and where they show: status, plan_view, the dashboard and the inbox."""

from sluice import plan as P
from sluice.registry import BUILTIN_DIR, load
from sluice.views import mermaid
from tests.conftest import create
from tests.test_dashboard import get

REG = load({"builtin": [BUILTIN_DIR]})


def echo(ref, doc=None):
    step = {"run": "core.echo", "in": {"value": {"source": ref}}}
    return {**step, "doc": doc} if doc is not None else step


def test_input_and_step_docs_validate():
    doc = {"inputs": {"who": {"type": "string", "doc": "Who signs off"},
                      "n": {"type": "int?"}, "tags": "string[]",
                      "mode": {"type": {"type": "enum", "symbols": ["a", "b"]}, "doc": "x"}},
           "outputs": {}, "steps": {"s": echo("who", "Echo the name")}}
    errs, plan = P.validate(doc, REG)
    assert errs == []
    assert [str(plan.inputs[k]) for k in ("who", "n", "tags")] == ["string", "int?", "string[]"]
    assert plan.input_docs == {"who": "Who signs off", "mode": "x"}
    assert plan.steps["s"].doc == "Echo the name"


def test_bad_docs_are_errors_with_paths():
    doc = {"inputs": {"a": {"doc": "no type"}, "b": {"type": "strng", "doc": "typo"},
                      "c": {"type": "string", "doc": 3}},
           "outputs": {}, "steps": {"s": echo("c", ["not", "text"])}}
    errs, _ = P.validate(doc, REG)
    assert errs == ["inputs.a.type: required", "inputs.b.type: unknown type 'strng'",
                    "inputs.c.doc: expected a string", "steps.s.doc: expected a string"]
    errs, _ = P.validate({"inputs": {"d": {"type": "string", "why": "x"}}, "outputs": {},
                          "steps": {}}, REG)
    assert len(errs) == 1 and errs[0].startswith("inputs.d: not a type")


def test_status_and_plan_view_show_the_docs(store):
    create(store, "p", {"s": echo("who", 'Say "hi" <b>loudly</b> #1'), "t": echo("who")},
           inputs={"who": {"type": "string", "doc": "Who signs off"}, "n": "int?"})
    st = store.status("p")
    assert st["input_docs"] == {"who": "Who signs off"}
    assert st["inputs"] == {"who": None, "n": None}
    steps = {s["id"]: s for s in st["steps"]}
    assert steps["s"]["doc"] == 'Say "hi" <b>loudly</b> #1' and "doc" not in steps["t"]
    store.set_input("p", "who", "Sam", "test", "")  # typed by the object form's type
    text = mermaid(store.plan("p")[1], store.read_state("p"))
    assert ('s0["s / core.echo / pending<br/>Say #quot;hi#quot; #lt;b#gt;loudly#lt;/b#gt; '
            '#35;1"]') in text
    assert 's1["t / core.echo / pending"]' in text
    create(store, "q", {"s": echo("x")}, inputs={"x": "string"})
    assert "input_docs" not in store.status("q")


def test_the_project_page_shows_the_docs(store, port):
    create(store, "p", {"s": echo("who", "Echo <i>it</i>")},
           inputs={"who": {"type": "string", "doc": "Who <b>signs</b> off"}})
    page = get(port, "/projects/p")[1]
    assert "<th>doc</th>" in page
    assert '<td class="muted">Who &lt;b&gt;signs&lt;/b&gt; off</td>' in page
    assert '<div class="muted">Echo &lt;i&gt;it&lt;/i&gt;</div>' in page


def test_an_input_item_without_a_body_takes_the_input_doc(store):
    create(store, "p", {}, inputs={"who": {"type": "string", "doc": "Who signs **off**"}})
    assert store.inbox_post("p", "Who?", input="who")["body"] == "Who signs **off**"
    assert store.inbox_post("p", "Who?", body="mine", input="who")["body"] == "mine"
    assert "body" not in store.inbox_post("p", "Who?")


def test_the_inbox_lists_inputs_that_hold_up_a_step(store, port):
    create(store, "p", {"s": echo("who"), "t": echo("n"), "u": echo("free")},
           inputs={"who": {"type": "string", "doc": "Who signs <b>off</b>"},
                   "n": "int?", "free": "string"})
    store.set_input("p", "free", "x", "test", "")
    assert store.waiting_inputs() == [{"project": "p", "name": "who", "type": "string",
                                       "steps": ["s"], "doc": "Who signs <b>off</b>"}]
    page = get(port, "/inbox")[1]
    assert "Waiting on a person" in page and "<code>who</code>" in page
    assert "Who signs &lt;b&gt;off&lt;/b&gt;" in page
    assert "Waiting on a person" not in get(port, "/inbox?status=all")[1]
    i = store.inbox_post("p", "Who?", input="who")["id"]  # an open item asks for it now
    assert store.waiting_inputs("p") == []
    store.inbox_close("p", i, None, "test")
    assert [w["name"] for w in store.waiting_inputs("p")] == ["who"]
    store.set_input("p", "who", "Sam", "test", "")
    assert store.waiting_inputs() == []
    assert "Waiting on a person" not in get(port, "/projects/p/inbox")[1]
