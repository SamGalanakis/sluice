"""Datastar component lifecycle and idle-work regressions in real Chromium."""

import json

import pytest

from tests.browser import Chrome, find_chrome
from tests.conftest import add, create, d, src


@pytest.fixture
def chrome():
    binary = find_chrome()
    if binary is None:
        pytest.skip("no Chromium (set SLUICE_CHROME)")
    browser = Chrome(binary)
    yield browser
    browser.close()


def open_board(store, port, chrome):
    create(store, "p", {"a": add(d(1), d(2)), "b": add(src("a/sum"), d(2)),
                        "c": add(d(1), d(2))})
    chrome.open(f"http://127.0.0.1:{port}/projects/p")
    chrome.send("Emulation.setDeviceMetricsOverride", {
        "width": 1440, "height": 1000, "deviceScaleFactor": 1, "mobile": False})
    chrome.wait("!!window.sluiceStream && document.querySelectorAll('.wires path').length > 0")
    chrome.eval("""window.draws = 0;
      new MutationObserver(() => window.draws++).observe(document.querySelector('svg.edges'),
                                                       {childList: true});""")


def pause(chrome, milliseconds=250):
    chrome.eval(f"new Promise(resolve => setTimeout(resolve, {milliseconds}))")


def idle(chrome):
    pause(chrome)
    before = chrome.eval("window.draws")
    pause(chrome, 400)
    assert chrome.eval("window.draws") == before


def listeners(chrome, expression, types):
    obj = chrome.send("Runtime.evaluate", {"expression": expression})["result"]["objectId"]
    found = chrome.send("DOMDebugger.getEventListeners", {"objectId": obj})["listeners"]
    return {kind: sum(item["type"] == kind for item in found) for kind in types}


def scoped_patch(chrome, selector):
    """New descendants still receive Datastar plugins and Rocket's local signal scope."""
    chrome.eval(f"""(() => {{
      const child = document.createElement('div');
      child.innerHTML = `<button data-signals="{{probe: 0}}" data-on:click="$$probe++">Probe</button>
                         <span data-text="$$probe"></span>`;
      const host = document.querySelector({json.dumps(selector)});
      host.append(child);
      // Datastar emits this event after morphing server patches into scoped descendants.
      host.dispatchEvent(new CustomEvent("datastar-scope-children"));
      window.probeChild = child;
    }})()""")
    chrome.wait("window.probeChild.querySelector('span').textContent === '0'")
    chrome.eval("window.probeChild.querySelector('button').click()")
    chrome.wait("window.probeChild.querySelector('span').textContent === '1'")
    assert chrome.eval("import(document.querySelector('script[src*=datastar-rocket]').src)"
                       ".then(m => m.getPath('probe') == null)")
    chrome.eval("window.probeChild.remove()")


def test_hover_keyboard_and_presentation_changes_settle(store, port, chrome):
    open_board(store, port, chrome)
    x, y = chrome.eval("""(() => { const r = document.querySelector('#n-a').getBoundingClientRect();
      return [r.left + r.width/2, r.top + r.height/2]; })()""")
    chrome.send("Input.dispatchMouseEvent", {"type": "mouseMoved", "x": x, "y": y})
    chrome.wait("document.querySelector('.plane').classList.contains('tracing')")
    assert chrome.eval("document.querySelector('.wires path').classList.contains('on')")
    idle(chrome)
    chrome.send("Input.dispatchMouseEvent", {"type": "mouseMoved", "x": 0, "y": 0})
    chrome.wait("!document.querySelector('.plane').classList.contains('tracing')")
    chrome.send("Input.dispatchKeyEvent", {"type": "keyDown", "key": "Tab", "code": "Tab"})
    chrome.eval("document.querySelector('#n-a').focus()")
    chrome.wait("document.querySelector('#n-a').matches(':focus-visible')")
    chrome.send("Input.dispatchKeyEvent", {
        "type": "keyDown", "key": "ArrowDown", "code": "ArrowDown"})
    assert chrome.eval("document.activeElement.id") == "n-b"
    idle(chrome)
    chrome.eval("document.querySelector('#n-a').classList.add('open'); "
                "document.querySelector('#n-b .g').classList.add('flip')")
    idle(chrome)


def test_reconnect_removes_listeners_aborts_drawer_and_cancels_callbacks(store, port, chrome):
    open_board(store, port, chrome)
    board_types = ["pointerover", "pointerout", "focusin", "focusout", "keydown", "toggle"]
    expected = listeners(chrome, "document.querySelector('sluice-board')", board_types)
    assert set(expected.values()) == {1}
    scroll = listeners(chrome, "document.querySelector('#drawer')", ["scroll"])
    for _ in range(3):
        chrome.eval("""(() => {
          window.controller = window.sluiceStream();
          const board = document.querySelector('sluice-board'), parent = board.parentNode;
          board.remove(); parent.append(board);
          const host = document.querySelector('sluice-drawer'), next = host.nextSibling;
          window.drawerParent = host.parentNode; window.drawerNext = next; window.drawerHost = host;
          host.remove();
        })()""")
        assert chrome.eval("window.controller.signal.aborted")
        chrome.eval("window.drawerParent.insertBefore(window.drawerHost, window.drawerNext)")
        assert listeners(chrome, "document.querySelector('sluice-board')", board_types) == expected
        assert listeners(chrome, "document.querySelector('#drawer')", ["scroll"]) == scroll
    chrome.eval("""window.late = 0;
      document.querySelector('#drawer').focus = () => window.late++;
      document.querySelector('#n-a').scrollIntoView = () => window.late++;
      history.replaceState(null, '', '#step:a');
      window.dispatchEvent(new HashChangeEvent('hashchange'));
      window.drawerHost.remove();""")
    pause(chrome, 350)
    assert chrome.eval("window.late") == 0
    assert chrome.eval("!document.documentElement.classList.contains('drawer-open')")


def test_server_status_geometry_and_scoped_descendants_still_update(store, port, chrome):
    open_board(store, port, chrome)
    store.update_step("p", "b", {"doc": "Patched card"}, "test", "edit")
    chrome.wait("document.querySelector('#n-b')?.getAttribute('aria-description')?.startsWith('Patched card')")
    store.set_output("p", "a", {"sum": 3}, "test", "finished")
    chrome.wait("document.querySelector('#n-a')?.classList.contains('is-manual')")
    chrome.wait("document.querySelector('#announce').textContent.includes('a set by hand')")
    idle(chrome)
    before = chrome.eval("window.draws")
    chrome.send("Emulation.setDeviceMetricsOverride", {
        "width": 390, "height": 900, "deviceScaleFactor": 1, "mobile": False})
    chrome.wait("document.querySelector('svg.edges').childElementCount === 0")
    assert chrome.eval("window.draws") > before
    chrome.send("Emulation.setDeviceMetricsOverride", {
        "width": 1440, "height": 1000, "deviceScaleFactor": 1, "mobile": False})
    chrome.wait("document.querySelectorAll('.wires path').length > 0")
    store.set_output("p", "b", {"sum": 5}, "test", "finished")
    chrome.wait("!!document.querySelector('details.fold-box')")
    chrome.eval("document.querySelector('details.fold-box').open = true")
    chrome.wait("!!document.querySelector('#n-a') && document.querySelectorAll('.wires path').length > 0")
    chrome.eval("document.querySelector('details.fold-box').open = false")
    chrome.wait("document.querySelectorAll('.wires path').length === 0")
    chrome.eval("document.querySelector('details.fold-box').open = true")
    chrome.wait("document.querySelectorAll('.wires path').length > 0")
    idle(chrome)
    scoped_patch(chrome, "sluice-board")


def test_thread_reconnect_and_patched_descendants(store, port, chrome):
    create(store, "p", {})
    store.append("p", {"kind": "message", "thread": "t", "from": "agent", "body": "First"})
    chrome.open(f"http://127.0.0.1:{port}/projects/p/threads")
    chrome.wait("!!document.querySelector('sluice-thread')?.hasAttribute('data-scope-children')")
    initial = listeners(chrome, "document.querySelector('sluice-thread')", ["toggle"])
    for _ in range(3):
        chrome.eval("""(() => { const host = document.querySelector('sluice-thread');
          const parent = host.parentNode; host.remove(); parent.append(host); })()""")
        assert listeners(chrome, "document.querySelector('sluice-thread')", ["toggle"]) == initial
    scoped_patch(chrome, "sluice-thread")
    chrome.eval("document.querySelector('details.thread').open = false")
    pause(chrome)
    store.append("p", {"kind": "message", "thread": "t", "from": "agent", "body": "Second"})
    chrome.wait("document.querySelector('sluice-thread .th-new').textContent === '1 new'")
    chrome.eval("document.querySelector('details.thread').open = true")
    chrome.wait("document.querySelector('sluice-thread .th-new').textContent === ''")


def test_unchanged_clocks_and_irrelevant_mutations_do_no_global_rescans(store, port, chrome):
    chrome.open("about:blank")
    chrome.send("Page.enable")
    chrome.send("Page.addScriptToEvaluateOnNewDocument", {"source": """
      window.clockCallbacks = [];
      const interval = window.setInterval;
      window.setInterval = (fn, delay, ...args) => {
        if (delay === 5000) { window.clockCallbacks.push(fn); return 0; }
        return interval(fn, delay, ...args);
      };
    """})
    chrome.send("Page.navigate", {"url": f"http://127.0.0.1:{port}/"})
    chrome.wait("window.clockCallbacks?.length === 1")
    assert chrome.eval("""(async () => {
      const wrap = document.body.appendChild(document.createElement('div'));
      Date.now = () => Date.parse('2026-01-01T02:00:00Z');
      wrap.innerHTML = '<time data-since="2026-01-01T00:00:00Z">2h</time>' +
        '<time data-ago datetime="2026-01-01T01:00:00Z">1h ago</time>' +
        '<span data-quiet="2026-01-01T01:00:00Z"><span class="qt">quiet 1h</span></span>';
      await new Promise(resolve => setTimeout(resolve, 50));
      let mutations = 0;
      const observer = new MutationObserver(records => mutations += records.length);
      observer.observe(wrap, {childList:true, subtree:true, attributes:true});
      window.clockCallbacks[0](); window.clockCallbacks[0]();
      await new Promise(resolve => setTimeout(resolve, 50));
      observer.disconnect(); return mutations;
    })()""") == 0
    assert chrome.eval("""(async () => {
      let scans = 0;
      const query = document.querySelectorAll.bind(document);
      document.querySelectorAll = selector => {
        if (selector === '.types-toggle') scans++;
        return query(selector);
      };
      const single = document.querySelector.bind(document);
      document.querySelector = selector => {
        if (selector === '[data-title-failed]') scans++;
        return single(selector);
      };
      const svg = document.body.appendChild(document.createElementNS('http://www.w3.org/2000/svg','svg'));
      svg.appendChild(document.createElementNS(svg.namespaceURI,'path'));
      document.querySelector('time[data-ago]').textContent = 'same';
      await new Promise(resolve => setTimeout(resolve, 50));
      return scans;
    })()""") == 0
    chrome.eval("document.querySelector('[data-title-failed]')?.remove()")
    chrome.eval("document.body.insertAdjacentHTML('beforeend', "
                "'<button class=types-toggle aria-pressed=true>Types</button>' + "
                "'<div data-title-failed=2></div>')")
    chrome.wait("document.querySelector('.types-toggle').getAttribute('aria-pressed') === 'false'")
    chrome.wait("document.title.startsWith('2 failed')")
    chrome.eval("document.querySelector('[data-title-failed]').dataset.titleFailed = '3'")
    chrome.wait("document.title.startsWith('3 failed')")
    chrome.eval("document.querySelector('[data-title-failed]').remove()")
    chrome.wait("!document.title.includes('failed')")
    chrome.eval("document.querySelector('.types-toggle').setAttribute('aria-pressed', 'true')")
    chrome.wait("document.querySelector('.types-toggle').getAttribute('aria-pressed') === 'false'")
