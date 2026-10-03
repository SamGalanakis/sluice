# Datastar and Rocket efficiency audit

Checked 2026-10-03 against fetched `origin/main`, commit
`dcea29a7ecb21e9f78ebbb3f46d3ff2a887a7a56`. The worktree was clean and already on
that commit. Findings below describe that baseline. The accepted fixes and their
verification are recorded in the implementation section at the end.

Sluice uses Datastar appropriately at the architectural level. Its largest
confirmed inefficiency is a board observer feedback loop in our JavaScript.
There are also avoidable lifecycle and log costs. Keep the server-rendered
dashboard and SSE design; fix the work around it.

## What we ship and what we read

The frontend is Datastar 1.0.4 with Rocket beta.2. Our 65,470-byte bundle matches
the [official versioned bundle](https://cdn.jsdelivr.net/gh/starfederation/datastar@v1.0.4/bundles/datastar-rocket.js)
byte for byte. SHA256 is
`c723fb309157a555609df273e4693dcc4e193e68e6d7477f3285b955288e1e7b`.
Datastar 1.0.4 is the current stable frontend release, checked through GitHub's
live releases API. Rocket remains beta. The audited Python SDK was 1.0.2; stable 1.0.3
was available and is now installed. Its Starlette adapter is unchanged and its SSE generator changes
are typing changes, so upgrading is maintenance, with no demonstrated throughput
benefit. [Frontend release](https://github.com/starfederation/datastar/releases/tag/v1.0.4),
[SDK comparison](https://github.com/starfederation/datastar-python/compare/v1.0.2...v1.0.3).

The review covered the getting-started and backend guides, the Tao, attribute,
action, Rocket and SSE references, interruption guidance, and the Active Search,
Lazy Load, Copy Button, ECharts, Flow and Virtual Scroll examples. Recommendations
were checked against the source map of the exact shipped bundle rather than
assuming every current website example has the same contract.

## What is already a good fit

The [Tao of Datastar](https://data-star.dev/guide/the_tao_of_datastar) recommends
backend-owned state, sparse interaction signals, ordinary page links, morphing
HTML, and a long-lived read request alongside short writes. Sluice follows that
shape. Sending a substantial HTML part is supported by the framework and does
not mean it replaces every descendant in the browser. Server rendering and
network transmission still have costs independent of morphing.

In particular:

- Pages initially render useful HTML, with forms and links that work without JS.
- `_parts_stream` checks versions and only sends parts whose HTML differs from
  its connection's baseline. SQLite reads are short snapshots; polling runs off
  the event loop. The snapshot is released between polls.
- Project streams already reuse rendered parts when only log records change.
  Finished boxes defer their cards and have stale-response checks.
- Log filters use binding and a 300 ms debounce, comparable to the official
  [Active Search example](https://data-star.dev/examples/active_search).
- Rocket light-DOM wrappers are appropriate for enhancing our server-rendered
  children. Rocket explicitly supports omitting `render`.
- Importing the same Rocket module from the page and `sluice.js` does not create
  two runtimes. Browser ES modules share evaluation for the same URL.
- GET streams retain Datastar's hidden-tab disconnection default. The drawer's
  custom AbortController is useful because switching steps changes the URL;
  automatic cancellation only cancels the same method and URL.

The small version, cursor and filter signals do not warrant blanket filtering.
The [backend guide](https://data-star.dev/guide/backend_requests) deliberately
sends all non-private signals by default and discourages partial filtering
without a reason.

## Findings, in fix order

### 1. Hover causes continuous full edge redraws

High priority, reproduced in headless Chromium at 1440 px on a two-card board.
With no backend changes, a stationary pointer over a card caused **121 SVG
replacements in 2.01 seconds**. Each replacement comes from `drawEdges`, which
measures the cards, computes routes, creates paths and labels, and replaces the
entire edge SVG.

The cycle is in
[trace and the board observer](https://github.com/SamGalanakis/sluice/blob/dcea29a7ecb21e9f78ebbb3f46d3ff2a887a7a56/src/sluice/static/sluice.js#L252):

1. `trace` calls `.plane.classList.add('tracing')` unconditionally.
2. The subtree MutationObserver watches class mutations and schedules `redraw`.
3. `redraw` draws all edges and calls `trace` again for the hovered card.
4. Adding the existing class still produces an observed mutation, repeating the
   cycle at animation-frame frequency.

A second scratch-browser experiment guarded just that redundant class addition.
Redraws fell from **121 to 0 over two seconds**, after settling, with the pointer
still on the card. This establishes the cause without changing application code.

Fix the idempotence error and have the observer ignore presentation-only
`tracing`, `near`, `open` and `flip` changes unless they actually require layout.
Keep status announcements tied to real status changes. The frame scheduler
should coalesce genuine invalidations and stay idle after hover/focus settles.
Retain SVG ownership through `data-ignore-morph`.

Acceptance evidence should cover stationary hover and keyboard focus, pointer
exit, status patches, resize, and box expansion. Assert that redraw counts stop
growing after settling; preserve highlighting and accessibility announcements.

### 2. Component reconnection duplicates our event handlers

Medium priority, confirmed with DevTools `DOMDebugger.getEventListeners`.
Removing and reinserting the same `sluice-board` element changed every host
pointer, focus, keydown and toggle listener count from **1 to 2**. Rocket's own
scoping listener stayed at 1.

[Board cleanup](https://github.com/SamGalanakis/sluice/blob/dcea29a7ecb21e9f78ebbb3f46d3ff2a887a7a56/src/sluice/static/sluice.js#L418)
disconnects observers and removes the media-query listener, but leaves host
listeners installed. Thread cleanup also leaves its host toggle listener, and
drawer cleanup leaves its scroll listener. The drawer deletes its global stream
helper without explicitly aborting the active controller.

Rocket calls setup again after reconnect. It runs its internal teardown and
registered application cleanups, but does not remove arbitrary application listeners. The official
[Copy Button example](https://data-star.dev/examples/rocket_copy_button)
explicitly removes its host listener. The
[versioned runtime](https://github.com/starfederation/datastar/blob/v1.0.4/library/src/rocket/runtime.ts#L1579)
confirms this contract.

Use named handlers with corresponding removals, or instance-owned abortable
event listeners. Abort the drawer's stream, and clear pending callbacks, on
disconnect. Test repeated same-node reconnect and one action per event. This is
a demonstrated reconnection bug, not evidence that every ordinary replacement
leaks memory: discarded elements and their listeners can be garbage-collected.

### 3. Live log tables grow without a bound

Medium priority for long-running sessions. The first page uses `PAGE_SIZE = 50`,
but [_log_stream](https://github.com/SamGalanakis/sluice/blob/dcea29a7ecb21e9f78ebbb3f46d3ff2a887a7a56/src/sluice/dashboard.py#L281)
reads every matching record since `seen` without a batch limit and prepends all
rows. It never evicts old rows or refreshes the pager as the window grows.

In Chromium, a page with 2 rows reached **122 rows after 120 new records**.
Database retention caps do not remove already-rendered browser rows. A long
session can therefore accumulate far more than 50 rows even if storage remains
bounded. Large reconnect gaps also produce large single renders and patches.

Choose an explicit live-window policy. A bounded recent table with an indication
that older records remain available through pagination is simpler than a full
virtualized table. Batch catch-up reads and preserve correct `seen` advancement
when filters exclude records. Test reconnect after a gap, interruptions between
row and cursor events, and filter changes during catch-up. Do not advance the
cursor past records that were not delivered under the chosen policy.

The official [Rocket Virtual Scroll example](https://data-star.dev/examples/rocket_virtual_scroll)
shows an alternative using three recycled buffers. Borrow its bounded-DOM
principle; its entire implementation is not necessary for our log table.

### 4. Log version checks scan retained history every second

Medium-to-low priority at the default 10,000-record cap, higher with many visible
tabs or raised retention.
[log_ver](https://github.com/SamGalanakis/sluice/blob/dcea29a7ecb21e9f78ebbb3f46d3ff2a887a7a56/src/sluice/dashboard.py#L209)
uses `SELECT max(seq), count(*) ... WHERE project IS ?`. The covering index
avoids reading payloads, but `count(*)` still scans all matching entries. This
happens even when the page receives no events.

| Retained records | Current version check, median | `max(seq)` alone, median |
| --- | ---: | ---: |
| 100 | 0.019 ms | 0.003 ms |
| 10,000 | 0.399 ms | 0.004 ms |
| 100,000 | 3.935 ms | 0.004 ms |

The 100,000 case is an intentionally raised-retention scenario. The current
default cost is modest for one viewer. The measurements establish linear work,
not a production outage.

Use an inexpensive append/trim revision, or maintained log metadata. Do not
replace the query with max alone without preserving trim detection. Reusing the
general project version is cheap but also wakes the log on unrelated writes.
Include global records where `project IS NULL` in the design.

### 5. Versions invalidate more than each view needs

Medium-to-low priority, source-confirmed. Every project write moves its general
version. The Threads stream and step drawer use it, and the inbox uses versions
from every project. An unrelated state/log change can therefore rerender an
entire thread panel, step detail or inbox before HTML equality suppresses most
outgoing patches.

The project board already has a second, visible-data cache key in
`_project_parts`. Extend that approach where it pays off. Separate message,
inbox and step-detail dependencies, and cache expensive view parts with all of
their actual dependencies, including registry files and stderr. Preserve the
consistent snapshot and external-file recheck.

Costs are connection-local. Multiple visible viewers repeat polling and
rendering, and `_parts_stream` initially renders a baseline even if the client's
version already matches. A shared watcher/cache may help at larger fanout, but
it is more work than the immediate fixes. Process-local commit notifications
alone cannot detect writes from other CLI/agent processes or file changes.

Our small-home idle version checks were cheap: 1 SELECT for index, 2 for home,
3 for project and 4 for a step, taking roughly 0.03–0.06 ms. A 1,000-step idle
project snapshot took 0.084 ms. Keep the one-second fallback unless actual
viewer/load measurements justify replacing it.

### 6. Timers and broad observers perform avoidable browser work

Low priority after fixing the hover loop. `tick` unconditionally assigns
`textContent` to every relative/running time every five seconds. One assignment
of an identical running-time string caused **one full board redraw**. During an
11-second idle interval, the running board redrew twice for its clock updates.
Actual duration changes can affect geometry, but unchanged strings need no
mutation.

Add equality guards to time updates. Narrow the Types and title observers to
relevant added/changed elements; both currently scan the page on broad subtree
mutation batches, including our SVG replacements. The inbox observer is more
selective and `data-ignore-morph` protects typed answers; keep that ownership.

Board and thread components omit `render`, use `observeProps`, and still inherit
`renderOnPropChange: true`. The shipped
[Rocket runtime](https://github.com/starfederation/datastar/blob/v1.0.4/library/src/rocket/runtime.ts#L968)
then queues a no-render pass which indexes descendants. Consider
`renderOnPropChange: false`, as in the official
[ECharts example](https://data-star.dev/examples/rocket_echarts), after verifying
that newly patched descendants still receive the ownership/scoping they need.
This does not eliminate our MutationObserver work. Its time impact was not
benchmarked separately.

## Large HTML patches and transport

A synthetic plan with independent chains of ten pending steps measured:

| Steps | Render all project parts, median | Graph HTML bytes |
| --- | ---: | ---: |
| 10 | 1.692 ms | 5,476 |
| 100 | 6.006 ms | 50,987 |
| 300 | 14.202 ms | 151,907 |
| 1,000 | 49.460 ms | 505,128 |

Any visible change that changes the graph sends that whole graph part. Datastar
can morph it correctly, but cannot erase its server-render or wire cost.
Our existing deferred finished boxes already reduce this burden. Measure real
large active plans before adding fine-grained card patches; their complexity
must earn its cost.

The Tao recommends stream compression. The audited server registered no
compression middleware and static files used `Cache-Control: no-cache`, meaning
they can revalidate rather than necessarily redownload. Versioned vendor assets
can use long-lived immutable caching; unversioned application JS/CSS require
content-versioned URLs first. Standard installed Starlette GZip middleware
explicitly excludes `text/event-stream`, so adding it alone would not compress
our streams. Any SSE compression must flush promptly. For a loopback dashboard,
address unnecessary computation before transport tuning.

## Retry and simplification decisions

`retry: 'always'` allows reconnect after a clean stream end, including deployment
shutdown. Datastar's default `auto` does not retry clean EOF. Keep that recovery
behavior. However, our million-attempt limit also retries permanent 404/500
responses; normal streams cap the wait at three seconds. Use a terminal outcome
for missing/deleted resources and bounded/backoff behavior for persistent
errors. The exact behavior is in the
[1.0.4 fetch source](https://github.com/starfederation/datastar/blob/v1.0.4/library/src/plugins/actions/fetch.ts)
and [actions reference](https://data-star.dev/reference/actions).

The log filter's new request cancels the previous request even though the initial
request comes from `main` and later ones from the form: automatic cancellation
is keyed by method and URL across elements. There is no duplicate-stream bug
from that element difference. The drawer controller remains necessary across
different step URLs.

The drawer comment claiming an ordinary attribute change restarts all Datastar
attributes is stale for the shipped runtime. Its engine ignores ordinary class
mutations and reapplies the specific changed Datastar attribute. Update that
explanation when touching the code; do not retain it as a framework constraint.

Keep the manual SVG renderer: the official
[Flow example](https://data-star.dev/examples/rocket_flow) is useful for lifecycle
and scheduling patterns, but is not a ready-made replacement for our dependency
layout and accessibility. Likewise, moving every small DOM interaction into
signals would add state without resolving these costs.

## Evidence and limits

`uv sync --locked` succeeded. The existing dashboard, inbox/browser and view
suites passed: **101 passed** with
`uv run pytest -q tests/test_dashboard.py tests/test_inbox_dashboard.py tests/test_views.py`.
These tests establish the current baseline; they do not test all the efficiency
conditions found above.

Browser probes used real headless Chromium and the repository's DevTools driver,
fresh synthetic homes, a no-runner server on an ephemeral loopback port, and
1440 × 1000 desktop metrics. The hover cause experiment modified only its
scratch browser runtime. No live sessions, settings or product data were used.

Warm Python timings are medians, 3 samples for full renders and 20 for log SQL,
and are descriptive local measurements rather than load-test guarantees. The
render plan shape, hardware/cache state and number of retained records matter.
No production CPU, memory or multi-viewer load measurements were taken.

Raw results are in [datastar-rocket-measurements.json](datastar-rocket-measurements.json).
## Implemented fixes

The accepted issues are fixed together with SPEC and DESIGN. The architecture remains
server-rendered HTML with Datastar 1.0.4 / Rocket beta.2; the Python SDK is 1.0.3.

- Board tracing excludes presentation-only class changes from geometry invalidation,
  schedules at most one pending redraw, and stops rewriting unchanged timer text.
- Board, drawer and thread teardown aborts host listeners, pending work and the drawer
  stream. Board/thread use `renderOnPropChange: false` with patched descendants scoped
  through the shipped Rocket event. Unread counts update after message morphing completes.
- Types and title observers inspect relevant mutations instead of rescanning the body
  for every SVG or timer change. Preference changes still survive server patches.
- Live logs morph a desired window of at most 50 visible rows and its pager. Reconnects
  send that desired window before their cursor, so a replay is idempotent. SQL applies
  suppressed thread-post call noise before pagination, retaining older-record access.
- Schema 6 adds scoped/kind log revision counters maintained by insert/update/delete
  triggers, including cached writers from before migration. Append, correction and trim
  detection no longer scans retained records.
- Thread, inbox and step versions track their visible dependencies. Run activity includes
  directory fallback, stderr, input and exit files. Project/index versions also notice
  function-file edits without a database write. Registry fingerprint checks add filesystem
  work (about 1 ms in this synthetic test home) in exchange for correct external updates.
- Streams retain clean-EOF recovery with ten retries and a 30-second maximum wait.
  Missing/deleted resources return terminal 204. Gzip is negotiated and flushed for every
  SSE event; MCP transport is unchanged.
- Static assets have content-versioned URLs, matching local imports, immutable cache
  headers and ETags. The server snapshots the exact bytes corresponding to the version;
  bare and stale-version URLs revalidate.

The repeated scratch probe measured **zero SVG replacements and zero animation-frame
requests during two seconds of stationary hover**, versus 121 replacements before. Host
listener counts remain one per event after same-node reconnection. After 120 new log
records, the browser holds **50 rows**, versus 122 before. At 100,000 retained records,
`log_ver` took **0.033 ms** (20-sample warm median), versus **3.935 ms** in the baseline;
its query plan is an indexed lookup of `log_revisions`. These are descriptive measurements
on synthetic local data, not production load guarantees. Raw after-results are in
[datastar-rocket-measurements-after.json](datastar-rocket-measurements-after.json).

Validation: `uv run pytest -q` reports **1,000 passed, 7 skipped**. The five new Chromium
regressions cover hover/keyboard settling, lifecycle cleanup, real server status patches,
resize/fold behavior, Rocket local scope, thread unread updates, timer idempotence and
observer relevance. Independent review found no actionable defect; its focused database
and dashboard run passed 56 tests. Ruff and diff checks pass.

All 78 screenshots were captured and inspected: 13 page variants at 390, 1440 and
2560 pixels in light and dark themes. A separate scratch-browser check confirmed
aligned navigation and content edges, centered columns and no horizontal overflow
across the same 78 combinations. No clipped content was found.

Graph parts remain whole server-rendered fragments. Shared stream fanout and per-card
patching remain optional future work requiring measured multi-viewer/large-plan demand;
no speculative architecture change is needed to resolve these findings.
