"""Fn icons (SPEC §4, §8): an icon.svg/png/webp in a fn's dir or a text `icon` in fn.json,
checked like the rest of fn.json, served by /fns/<name>/icon and shown on the board's cards, a
folded box's line, the step drawer and the Functions page."""

import hashlib
import re
import urllib.error
import urllib.request

import pytest

from sluice import registry as R
from sluice import views
from sluice.errors import InvalidPlan
from sluice.util import ICON_MAX
from sluice.verify import verify
from tests.conftest import create, d, src, write_fn

SVG = (b'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16">'
       b'<path d="M2 2h12" stroke="currentColor"/></svg>')
PNG = b"\x89PNG\r\n\x1a\n" + b"\x00" * 32
WEBP = b"RIFF\x10\x00\x00\x00WEBPVP8 " + b"\x00" * 8
CSP = "default-src 'none'; style-src 'unsafe-inline'; img-src data:"


def sha(data):
    return hashlib.sha256(data).hexdigest()


def fn_with(root, name, icon_file=None, data=b"", spec=None, **kw):
    fn_dir = write_fn(root, name, spec=spec, **kw)
    if icon_file:
        (fn_dir / icon_file).write_bytes(data)
    return fn_dir


def get(port, path, headers=None):
    req = urllib.request.Request(f"http://127.0.0.1:{port}{path}", headers=headers or {})
    try:
        r = urllib.request.urlopen(req, timeout=10)
    except urllib.error.HTTPError as e:
        return e.code, {k.lower(): v for k, v in e.headers.items()}, e.read()
    return r.status, {k.lower(): v for k, v in r.headers.items()}, r.read()


def problems(store, project=None):
    return verify(store, project)["problems"]


# ---- the registry -------------------------------------------------------------------------


def test_an_icon_file_in_the_fn_dir_is_its_icon(store, home):
    fns = home / "fns"
    fn_with(fns, "t.svg", "icon.svg", SVG)
    fn_with(fns, "t.png", "icon.png", PNG)
    fn_with(fns, "t.webp", "icon.webp", WEBP)
    fn_with(fns, "t.none")
    for name, kind, data in (("t.svg", "image/svg+xml", SVG), ("t.png", "image/png", PNG),
                             ("t.webp", "image/webp", WEBP)):
        icon = store.fn(name).icon
        assert (icon.type, icon.data, icon.hash, icon.text) == (kind, data, sha(data), "")
        assert store.fn(name).summary()["icon"] == {"kind": "image", "type": kind}
    assert store.fn("t.none").icon is None and "icon" not in store.fn("t.none").summary()
    listed = {x["name"]: x.get("icon") for x in store.registry().listing()}
    assert listed["t.svg"] == {"kind": "image", "type": "image/svg+xml"}
    assert listed["t.none"] is None


def test_a_text_icon_in_fn_json_and_the_file_wins(store, home):
    assert "icon" in R.KEYS
    fns = home / "fns"
    fn_with(fns, "t.text", spec={"icon": " 🔧 "})  # stripped, and no "unknown key"
    assert store.fn("t.text").icon == R.Icon(text="🔧")
    assert store.fn("t.text").summary()["icon"] == {"kind": "text", "text": "🔧"}
    fn_with(fns, "t.both", "icon.svg", SVG, spec={"icon": "🔧"})
    icon = store.fn("t.both").icon
    assert icon.data == SVG and icon.text == ""
    assert not problems(store)


@pytest.mark.parametrize(("file", "data", "spec", "message"), [
    ("icon.png", PNG + b"0" * ICON_MAX, None, "icon: icon.png is over 256 KB"),
    ("icon.png", SVG, None, "icon: icon.png is not a PNG image"),
    ("icon.svg", b"<html></html>", None, "icon: icon.svg is not an SVG image"),
    ("icon.webp", PNG, None, "icon: icon.webp is not a WebP image"),
    (None, b"", {"icon": "x" * 17}, "icon: a text icon is at most 16 characters"),
    (None, b"", {"icon": "a\nb"}, "icon: a text icon may not contain control characters"),
    (None, b"", {"icon": 3}, "icon must be a short text, e.g. an emoji"),
    (None, b"", {"icon": " "}, "icon must be a short text, e.g. an emoji"),
])
def test_a_bad_icon_is_a_fn_json_problem(store, home, file, data, spec, message):
    fn_dir = fn_with(home / "fns", "t.bad", file, data, spec=spec)
    assert {"where": "fns/t.bad/fn.json", "message": message} in problems(store)
    assert store.registry().get("t.bad") is None  # left out, like any broken global fn
    # in a project's own fns it blocks the project, as its other fn.json problems do
    create(store, "p", {})
    (store.project_dir("p") / "fns").mkdir(parents=True)
    fn_dir.rename(store.project_dir("p") / "fns" / "t.bad")
    with pytest.raises(InvalidPlan, match="function problems block"):
        store.usable_registry("p")


def test_two_icon_files_are_a_problem(store, home):
    fn_dir = fn_with(home / "fns", "t.two", "icon.svg", SVG)
    (fn_dir / "icon.png").write_bytes(PNG)
    assert {"where": "fns/t.two/fn.json",
            "message": "icon: icon.svg and icon.png are both there; keep one"} in problems(store)


def test_the_icon_follows_the_fn_lookup_finds(store, home):
    """Scopes as ever (SPEC §2): each project's own fn brings its own icon, and a project fn
    that reuses a global name collides, so lookup keeps the global fn and its icon."""
    fn_with(home / "fns", "t.shared", "icon.svg", SVG)
    create(store, "p", {})
    create(store, "q", {})
    fn_with(store.project_dir("p") / "fns", "own.tool", "icon.png", PNG)
    fn_with(store.project_dir("q") / "fns", "own.tool", spec={"icon": "🌊"})
    assert store.fn("own.tool", "p").icon.data == PNG
    assert store.fn("own.tool", "q").icon.text == "🌊"
    fn_with(store.project_dir("p") / "fns", "t.shared", spec={"icon": "🔧"})
    assert store.fn("t.shared", "p").icon.data == SVG
    assert any("collides" in x["message"] for x in problems(store, "p"))


def test_fn_save_takes_a_text_icon_and_reads_no_file(store, tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    (tmp_path / "icon.svg").write_bytes(b"not an svg")  # the cwd is never the fn's dir
    raw = {"name": "t.saved", "inputs": {}, "outputs": {}, "icon": "📦"}
    store.fn_save(raw, "print('{}')\n")
    assert store.fn("t.saved").icon.text == "📦"


def test_a_changed_icon_file_is_picked_up(store, home):
    fn_dir = fn_with(home / "fns", "t.svg", "icon.svg", SVG)
    assert store.fn("t.svg").icon.hash == sha(SVG)
    other = SVG.replace(b"M2 2h12", b"M2 8h12 ")
    (fn_dir / "icon.svg").write_bytes(other)  # the size changes, so the scan sees it
    assert store.fn("t.svg").icon.hash == sha(other)
    (fn_dir / "icon.svg").unlink()
    assert store.fn("t.svg").icon is None


def test_the_shipped_icons(store):
    """Built-ins: inbox.ask, thread.post/wait and core.external have one; the inline and
    core.* ones do not. Every shipped icon is a single-colour SVG drawn in currentColor."""
    reg = store.registry()
    with_icon = {n for n in reg.names() if reg.get(n).icon}
    assert {"inbox.ask", "thread.post", "thread.wait", "core.external"} <= with_icon
    assert not with_icon & {"core.echo", "core.collect", "core.format", "inline.bash",
                            "inline.python"}
    for f in [*R.BUILTIN_DIR.glob("*/icon.*"),
              *R.BUILTIN_DIR.parents[2].glob("packs/*/*/icon.*")]:
        text = f.read_text()
        assert f.name == "icon.svg" and 'viewBox="0 0 16 16"' in text, f
        assert 'stroke="currentColor"' in text and 'stroke-width="1.5"' in text, f
        assert not re.search(r'(fill|stroke)="#', text), f
    # core.external's is the board's external glyph
    ext = (R.BUILTIN_DIR / "core.external" / "icon.svg").read_text()
    assert re.search(r' d="([^"]+)"', views.GLYPHS["external"])[1] in ext


# ---- the route ----------------------------------------------------------------------------


def test_the_route_serves_a_builtin_a_global_and_a_project_fns_icon(store, home, port):
    builtin = (R.BUILTIN_DIR / "inbox.ask" / "icon.svg").read_bytes()
    code, headers, body = get(port, "/fns/inbox.ask/icon")
    assert (code, body) == (200, builtin)
    assert headers["content-type"].startswith("image/svg+xml")
    assert headers["x-content-type-options"] == "nosniff"
    assert headers["content-security-policy"] == CSP
    assert headers["etag"] == f'"{sha(builtin)}"'
    assert get(port, "/fns/inbox.ask/icon", {"If-None-Match": headers["etag"]})[0] == 304
    fn_with(home / "fns", "pack.tool", "icon.png", PNG)  # a pack's fn, installed globally
    code, headers, body = get(port, "/fns/pack.tool/icon")
    assert (code, body) == (200, PNG) and headers["content-type"].startswith("image/png")
    assert "content-security-policy" not in headers
    create(store, "p", {})
    fn_with(store.project_dir("p") / "fns", "own.tool", "icon.svg", SVG)
    code, headers, body = get(port, "/fns/own.tool/icon?project=p")
    assert (code, body) == (200, SVG) and headers["content-security-policy"] == CSP
    assert get(port, "/fns/pack.tool/icon?project=p")[0] == 200  # a project sees globals
    assert get(port, "/fns/own.tool/icon")[0] == 404  # not without its project


def test_the_route_404s_without_an_image_icon(store, home, port):
    fn_with(home / "fns", "t.text", spec={"icon": "🔧"})
    create(store, "p", {})
    for path in ("/fns/core.echo/icon", "/fns/t.text/icon", "/fns/no.such/icon",
                 "/fns/inbox.ask/icon?project=nope"):
        assert get(port, path)[0] == 404, path
    assert get(port, "/fns/inbox.ask/icon", {"Host": "evil.example:7420"})[0] == 403


def test_the_icons_url_changes_with_its_file(store, home, port):
    fn_dir = fn_with(home / "fns", "t.tool", "icon.svg", SVG)
    create(store, "p", {"s": {"run": "t.tool", "in": {}}})
    url = f"/fns/t.tool/icon?project=p&v={sha(SVG)}"
    assert url in views.project_page(store, "p", ver="x")
    other = SVG.replace(b"M2 2h12", b"M2 8h12 ")
    (fn_dir / "icon.svg").write_bytes(other)
    page = views.project_page(store, "p", ver="x")
    assert url not in page and f"/fns/t.tool/icon?project=p&amp;v={sha(other)}" in page
    assert get(port, f"/fns/t.tool/icon?project=p&v={sha(other)}")[2] == other


# ---- the views ----------------------------------------------------------------------------


def card(page, sid):
    m = re.search(rf'<(a|div) class="node (card|chip) [^"]*"[^>]* id="n-{sid}".*?</\1>', page,
                  re.DOTALL)
    assert m, f"no card for {sid}"
    return m[0]


def mask(name, data, size, project="p"):
    if size == "card":
        return (f'<span class="ficon fi-{size} fi-mask" data-fn="{name}" '
                'aria-hidden="true"></span>')
    q = f"project={project}&amp;" if project else ""
    return (f'<span class="ficon fi-{size} fi-mask" style="--fi:url(&quot;/fns/{name}/icon?'
            f'{q}v={sha(data)}&quot;)" aria-hidden="true"></span>')


def board(store, home):
    fns = home / "fns"
    fn_with(fns, "t.agent", "icon.svg", SVG, spec={"open": True})
    fn_with(fns, "t.pic", "icon.png", PNG, outputs={"x": "int"})
    fn_with(fns, "t.emoji", spec={"icon": "🧪"})
    create(store, "p", {
        "work": {"run": "t.agent", "in": {}},
        "pic": {"run": "t.pic", "in": {}},
        "emo": {"run": "t.emoji", "in": {}},
        "plain": {"run": "test.add", "in": {"a": d(1), "b": d(1)}}})


def test_a_card_shows_its_fns_icon_after_the_id(store, home):
    board(store, home)
    page = views.project_page(store, "p", ver="x")
    work = card(page, "work")
    assert f'<span class="sid">work</span>{mask("t.agent", SVG, "card")}' in work
    assert work.index('class="g g-pending"') < work.index("ficon")  # the glyph leads
    assert (f'<img class="ficon fi-card fi-img" src="/fns/t.pic/icon?project=p&amp;'
            f'v={sha(PNG)}" alt="">') in card(page, "pic")
    assert '<span class="ficon fi-card fi-text" aria-hidden="true">🧪</span>' in card(page, "emo")
    assert "ficon" not in card(page, "plain")  # a fn with none: the card as it was


def test_the_card_width_counts_the_icon(store, home):
    board(store, home)
    b = views.load_board(store, "p")
    blocks = b.blocks
    base = 50 + 7.7 * 4  # an id of four characters, nothing small
    assert views._card_width(b, blocks["work"]) == pytest.approx(base + 22)
    assert views._card_width(b, blocks["plain"]) == pytest.approx(50 + 7.7 * 5)
    assert views._card_width(b, blocks["emo"]) > 50 + 7.7 * 3


def test_the_drawer_head_and_the_functions_page_show_the_icon(store, home):
    board(store, home)
    head = views.step_detail(store, "p", "work").split("</header>")[0]
    assert (f'<p class="d-meta meta">{mask("t.agent", SVG, "meta")}'
            f'<code title="function">t.agent</code></p>') in head
    plain = views.step_detail(store, "p", "plain").split("</header>")[0]
    assert '<p class="d-meta meta"><code title="function">test.add</code></p>' in plain
    fns = views.fns_page(store)
    assert f'<div class="fn-head">{mask("t.agent", SVG, "full", None)}<b>t.agent</b>' in fns
    assert '<div class="fn-head"><span class="ficon fi-full fi-text" aria-hidden="true">' \
           '🧪</span><b>t.emoji</b>' in fns
    assert '<div class="fn-head"><b>core.echo</b>' in fns
    assert f'{mask("t.agent", SVG, "full")}<b>t.agent</b>' in views.fns_page(store, "p")


def test_a_folded_box_shows_its_main_fns_icon(store, home):
    fn_with(home / "fns", "t.agent", "icon.svg", SVG, spec={"open": True},
            outputs={"sum": "int"})
    create(store, "p", {
        "fork": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
        "work": {"run": "t.agent", "in": {"n": src("fork/sum")}},
        "close": {"run": "test.add", "in": {"a": src("work/sum"), "b": d(1)}},
        "other": {"run": "test.add", "in": {"a": d(1), "b": d(1)}}})
    ok = {"status": "succeeded", "outputs": {"sum": 2}}
    with store.tx():
        store.write_state("p", {"inputs": {}, "steps": {"fork": ok, "work": ok, "close": ok}})
    page = views.project_page(store, "p", ver="x")
    start = page.index('<li class="box done"')
    folded = page[start:page.index("</summary>", start)]
    assert f'<span class="sid">fork</span>{mask("t.agent", SVG, "card")}' in folded
    assert page.count('class="fold-box"') == 1


def test_a_ready_external_card_does_not_repeat_its_glyph(store):
    """A ready core.external step's glyph is already the external mark; before it is ready
    (a pending ring) its card shows the fn's icon like any other."""
    create(store, "p", {"first": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
                        "now": {"run": "core.external", "in": {}},
                        "later": {"run": "core.external", "in": {}, "after": ["first"]}})
    page = views.project_page(store, "p", ver="x")
    assert 'class="g g-external"' in card(page, "now") and "ficon" not in card(page, "now")
    assert "fi-card fi-mask" in card(page, "later")
    b = views.load_board(store, "p")
    assert views._card_width(b, b.blocks["later"]) - views._card_width(b, b.blocks["now"]) \
        == pytest.approx(22 + 7.7 * 2 - 8 - 6.7 * len("outside"))
