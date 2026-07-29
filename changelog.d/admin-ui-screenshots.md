### The admin console, documented in pictures — and four bugs it was hiding

The admin UI's documentation has always described 22 pages in prose and shown
none of them. It does now: every page in the console has a screenshot, generated
rather than hand-captured. `invoke admin-screenshots` starts a throwaway
SecantusDB on a fixed port, seeds it with a fictional shop — invented customers,
`example.com` addresses, public landmark coordinates, indexes of every shape,
users, profiler entries, backup archives — then drives all 22 pages through a
real browser with Playwright, filling and submitting forms where a bare page load
would only show an empty one. Machine-specific strings are rewritten out of the
DOM before each shot, so a committed image carries nothing about the machine that
made it. The same run publishes the four shots the marketing site uses, so the
docs, the README and secantusdb.com can't drift apart.

Driving the console through a real browser turned out to be the first time
anyone had. It found four live bugs, all invisible to the existing tests because
the templates render identically whether or not their JavaScript runs. Alpine was
loading before Chart.js, and since this Alpine build starts the moment its script
executes, the dashboard threw `Chart is not defined` during `init()` — which
aborted the component before it opened the metrics websocket. The dashboard has
been showing zeros, no charts, and a permanent "connecting…" status. Behind that
sat three more: every Alpine page called its own `init()` twice, so the
change-stream tail opened two sockets and displayed every event twice; the
sparkline canvases had no sized parent and grew until they filled the viewport;
and the geo map's markers 404'd on Leaflet image assets this package doesn't
vendor, so map pins rendered as broken images. All four are fixed, and each is
pinned by a regression test.

Regenerating the screenshots is now a release step. A browser-free test keeps
every documented page wired to an image on disk, but it can't tell a fresh
screenshot from a stale one — so the release procedure regenerates them, and the
capture itself fails loudly if any page logs a JavaScript error or is
photographed showing an empty state.

#### Added

- `scripts/admin_screenshots.py` and `invoke admin-screenshots`: Playwright-driven
  capture of all 22 admin-UI pages against a seeded throwaway server, with DOM
  anonymisation, JS-error detection, empty-state detection, and publication of
  the website-tagged subset into the Pelican theme. Flags: `--only`, `--list`,
  `--headed`, `--scale`, `--server-port`, `--keep-data`, `--skip-website`, and
  `--from-checkout` for rendering a working tree's templates and static assets
  instead of the installed package's.
- A `screenshots` optional extra carrying Playwright (kept out of `dev` so CI
  lanes don't install a browser stack they never drive).
- Screenshots throughout `docs/admin.md`, an admin-UI section in the README, and
  an admin console section on the secantusdb.com landing page.
- `tests/test_docs_screenshots.py` and `tests/test_admin_asset_order.py`.

#### Fixed

- **The admin dashboard never worked.** `alpine.min.js` loaded before
  `chart.umd.min.js`, and this Alpine build calls `Alpine.start()` as soon as its
  own deferred script runs, so `Chart` was undefined when the dashboard's
  `init()` executed. The thrown error aborted the component before `_connect()`,
  so the live-metrics websocket never opened: every tile read 0 and the status
  stayed on "connecting…". Alpine now loads last.
- Every Alpine page (`dashboard`, `changestream`, `query`, `insert`) carried a
  redundant `x-init="init()"` alongside a component that already defines
  `init()`, which Alpine invokes itself. Each page therefore initialised twice —
  two Chart instances per canvas, two metrics websockets, two change-stream
  sockets (so every event appeared twice), and duplicate collection-suggestion
  fetches.
- The dashboard's sparkline canvases had no fixed-height positioned parent, so
  Chart.js's `maintainAspectRatio: false` sizing loop grew each chart until it
  overflowed the viewport.
- Chart instances were stored in Alpine's reactive state; reached through its
  Proxy, Chart.js's internal per-chart lookups missed and every `update()` threw
  `Cannot set properties of undefined (setting 'fullSize')`. They now live in the
  component factory's closure.
- The geo map drew points with Leaflet's default marker, which loads
  `images/marker-icon.png` and `images/marker-shadow.png` relative to
  `leaflet.css` — files this package doesn't vendor. Every point 404'd twice and
  rendered broken; points are now vector `circleMarker`s needing no assets.

#### Changed

- `docs/admin.md` no longer claims the UI "never makes outbound network calls of
  its own". It makes one: the Geo page fetches basemap tiles from OpenStreetMap.
  That's now stated up front and called out in a note on the page's own section,
  since it tells a third party your IP and roughly where your data is.
