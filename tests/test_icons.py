"""Project icons (SPEC §2, §8): image files sniffed into icon.<ext>, text icons in
project.json, the /icon route, and the icon by a project's name on the dashboard."""

import re
import urllib.error
import urllib.request

import pytest

from sluice import views
from sluice.errors import BadRequest
from sluice.store import ICON_MAX

SVG = (b'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 4 4">'
       b'<rect width="4" height="4"/></svg>')
PNG = b"\x89PNG\r\n\x1a\n" + b"\x00" * 32


def write(tmp_path, name, data):
    p = tmp_path / name
    p.write_bytes(data)
    return str(p)


def icon_files(d):
    return sorted(f.name for f in d.iterdir() if f.name.startswith("icon."))


def get(port, path, host=None):
    req = urllib.request.Request(f"http://127.0.0.1:{port}{path}",
                                 headers={"Host": host} if host else {})
    try:
        r = urllib.request.urlopen(req, timeout=10)
    except urllib.error.HTTPError as e:
        return e.code, {k.lower(): v for k, v in e.headers.items()}, e.read()
    return r.status, {k.lower(): v for k, v in r.headers.items()}, r.read()


def test_an_image_icon_is_copied_in_and_replaced(store, tmp_path):
    svg, png = write(tmp_path, "a.svg", SVG), write(tmp_path, "a.png", PNG)
    store.create_project("p", "d", icon=svg)
    d = store.project_dir("p")
    assert (d / "icon.svg").read_bytes() == SVG and icon_files(d) == ["icon.svg"]
    assert store.icon("p") == {"kind": "image", "type": "image/svg+xml"}
    assert store.projects()[0]["icon"] == {"kind": "image", "type": "image/svg+xml"}
    store.update_project("p", icon=png)  # a new one replaces the old, whatever its extension
    assert (d / "icon.png").read_bytes() == PNG and icon_files(d) == ["icon.png"]
    assert store.icon("p") == {"kind": "image", "type": "image/png"}
    store.update_project("p", icon=svg)  # replacing a PNG with an SVG leaves one file
    assert icon_files(d) == ["icon.svg"]


def test_a_text_icon_lives_in_project_json_and_clears_the_image(store, tmp_path):
    store.create_project("p", "d", icon=write(tmp_path, "a.png", PNG))
    store.update_project("p", icon=" 🌊 ")  # stripped
    d = store.project_dir("p")
    assert icon_files(d) == [] and store.project("p")["icon"] == "🌊"
    assert store.icon("p") == {"kind": "text", "text": "🌊"}
    store.update_project("p", icon=write(tmp_path, "a.svg", SVG))  # an image clears the text
    assert "icon" not in store.project("p") and icon_files(d) == ["icon.svg"]
    store.update_project("p", icon="")  # the empty string removes any icon
    assert "icon" not in store.project("p") and icon_files(d) == []
    assert store.icon("p") is None and "icon" not in store.projects()[0]
    store.update_project("p", description="kept")  # no icon argument leaves it alone
    store.update_project("p", icon="🔧")
    store.update_project("p", description="still")
    assert store.project("p")["icon"] == "🔧"


def test_bad_icons_are_refused(store, tmp_path):
    store.create_project("p")
    with pytest.raises(BadRequest, match="not an SVG, PNG, WebP, JPEG or GIF"):
        store.update_project("p", icon=write(tmp_path, "x.txt", b"not an image"))
    with pytest.raises(BadRequest, match="over 256 KB"):
        store.update_project("p", icon=write(tmp_path, "big.png", PNG + b"0" * ICON_MAX))
    with pytest.raises(BadRequest, match="no readable file"):
        store.update_project("p", icon="/no/such/file.png")
    with pytest.raises(BadRequest, match="no readable file"):
        store.update_project("p", icon="~/no-such-file.png")
    with pytest.raises(BadRequest, match="at most 16 characters"):
        store.update_project("p", icon="x" * 17)
    with pytest.raises(BadRequest, match="control characters"):
        store.update_project("p", icon="a\nb")
    with pytest.raises(BadRequest, match="expected a string"):
        store.update_project("p", icon=3)
    assert store.icon("p") is None and icon_files(store.project_dir("p")) == []


def test_the_icon_route_serves_the_image_or_404s(store, port, tmp_path):
    store.create_project("p")
    assert get(port, "/projects/p/icon")[0] == 404  # no icon
    assert get(port, "/projects/nope/icon")[0] == 404  # no project
    store.update_project("p", icon="🌊")
    assert get(port, "/projects/p/icon")[0] == 404  # a text icon is not served here
    store.update_project("p", icon=write(tmp_path, "a.svg", SVG))
    code, headers, body = get(port, "/projects/p/icon")
    assert code == 200 and body == SVG
    assert headers["content-type"].startswith("image/svg+xml")
    assert headers["x-content-type-options"] == "nosniff"
    assert headers.get("etag") or headers.get("last-modified")
    assert headers["content-security-policy"] == \
        "default-src 'none'; style-src 'unsafe-inline'; img-src data:"
    assert get(port, "/projects/p/icon", host="evil.example:7420")[0] == 403
    store.update_project("p", icon=write(tmp_path, "a.png", PNG))
    code, headers, body = get(port, "/projects/p/icon")
    assert code == 200 and body == PNG and headers["content-type"].startswith("image/png")
    assert headers["x-content-type-options"] == "nosniff"
    assert "content-security-policy" not in headers


def test_the_dashboard_shows_the_icon_by_the_projects_name(store, tmp_path):
    store.create_project("p")
    store.create_project("q")
    assert 'class="picon"' not in views.index(store)
    store.update_project("p", icon="a<b")  # a text icon renders escaped
    store.update_project("q", icon=write(tmp_path, "q.svg", SVG))
    page = views.index(store)
    assert '<span class="picon" aria-hidden="true">a&lt;b</span>' in page
    assert '<span class="picon" aria-hidden="true">a<b</span>' not in page
    img = f'<img class="picon" src="/projects/q/icon?v=' \
          f'{store.icon_file("q").stat().st_mtime_ns}" alt="" width="20" height="20">'
    assert img in page
    board = views.project_page(store, "q", ver="x")
    assert '<link rel="icon" href="/projects/q/icon?v=' in board  # the favicon
    menu = re.search(r'<div class="menu">(.*?)</div></details>', board)[1]
    assert img in menu and "a&lt;b" in menu  # every menu entry
    summary = re.search(r"<summary[^>]*>(.*?)</summary>", board)[1]
    assert img in summary  # the switcher's button is the current project's icon + name
    store.create_project("r")  # a project with no icon: its name renders as before
    r = views.project_page(store, "r", ver="x")
    assert re.search(r"<summary[^>]*>(.*?)</summary>", r)[1].startswith(
        '<span class="sw-name">r</span>')
    standalone = views.project_page(store, "q")  # the standalone page head, no nav
    assert f'<div class="phead"><h1>{img}q</h1></div>' in standalone


def test_delete_project_removes_the_icon(store, tmp_path):
    store.create_project("p", icon=write(tmp_path, "a.svg", SVG))
    store.update_project("p", archived=True)
    store.delete_project("p")
    assert not store.project_dir("p").exists()
