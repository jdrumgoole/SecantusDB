"""Regression tests for the admin UI's front-end wiring.

Each of these pins a bug that a real browser found and no server-side
test could: the templates render identically whether or not the
JavaScript they emit actually runs. They were all discovered at once,
when ``scripts/admin_screenshots.py`` first drove the UI through
Chromium.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

TEMPLATES = Path(__file__).resolve().parent.parent / "src" / "secantus" / "admin" / "templates"
BASE = TEMPLATES / "base.html"
PAGES = TEMPLATES / "pages"

_SCRIPT_SRC_RE = re.compile(r'<script src="/static/js/([^"]+)"')
_JINJA_COMMENT_RE = re.compile(r"\{#.*?#\}", re.DOTALL)


def _markup(path: Path) -> str:
    """Template source with Jinja comments removed.

    The comments explain these very bugs and quote the offending markup,
    so a naive substring search would match the explanation and fail on a
    correctly-fixed file.
    """
    return _JINJA_COMMENT_RE.sub("", path.read_text(encoding="utf-8"))


def _script_order() -> list[str]:
    return _SCRIPT_SRC_RE.findall(_markup(BASE))


def test_alpine_loads_last() -> None:
    """Alpine must be the final deferred script in base.html.

    Deferred scripts run in document order, and the vendored Alpine build
    calls ``Alpine.start()`` the moment its own script executes — it
    registers no ``DOMContentLoaded`` hook. Anything an ``x-data`` /
    ``x-init`` expression reaches for (``Chart``, ``setupModal``,
    ``closeModal``) must therefore already be defined.

    With Alpine loaded second, the dashboard threw "Chart is not defined"
    during ``init()``, which aborted the component before it opened the
    metrics websocket: no charts, all tiles pinned at zero, status stuck
    on "connecting…".
    """
    scripts = _script_order()
    assert scripts, "no vendored scripts found in base.html"
    assert scripts[-1] == "alpine.min.js", (
        f"alpine.min.js must load last; current order is {scripts}"
    )


@pytest.mark.parametrize("page", sorted(p.name for p in PAGES.glob("*.html")))
def test_no_redundant_x_init(page: str) -> None:
    """No template calls its own component's ``init()`` from ``x-init``.

    Alpine already invokes a component's ``init()`` method when it sets up
    ``x-data``. The extra ``x-init="init()"`` ran it a second time, which
    on the dashboard built two Chart instances over each canvas ("Canvas
    is already in use") and opened two metrics sockets, on the
    change-stream page duplicated every event, and on query / insert
    fetched the collection suggestions twice.
    """
    text = _markup(PAGES / page)
    assert 'x-init="init()"' not in text, (
        f'{page}: drop x-init="init()" — Alpine calls the component\'s own init() already'
    )


def test_geo_points_need_no_image_assets() -> None:
    """The geo map draws vector markers, not Leaflet's default icon.

    ``leaflet.css`` refers to ``images/marker-icon.png`` and
    ``images/marker-shadow.png`` relative to itself. This package doesn't
    vendor those files, so the default ``L.marker`` 404'd twice per point
    and rendered every location as a broken image.
    """
    text = _markup(PAGES / "geo.html")
    assert "pointToLayer" in text and "circleMarker" in text, (
        "geo.html must render points via pointToLayer/circleMarker; the default "
        "marker needs image assets this package does not vendor"
    )


def test_dashboard_charts_are_not_alpine_reactive() -> None:
    """Chart instances stay out of Alpine's reactive state.

    Alpine proxies everything the component returns. Chart.js keys its
    per-chart plugin state off the raw chart object, so reached through a
    proxy those lookups miss: ``chart.legend`` comes back undefined and
    every ``update()`` threw "Cannot set properties of undefined (setting
    'fullSize')". Keeping the instances in the factory's closure keeps
    them un-proxied.
    """
    text = _markup(PAGES / "dashboard.html")
    assert "const charts = {}" in text, "dashboard charts must live in the closure"
    assert "this.charts" not in text, (
        "dashboard.html: chart instances must not be stored on the Alpine "
        "component — the reactive proxy breaks Chart.js's internal state lookups"
    )


def test_dashboard_canvases_have_a_sized_wrapper() -> None:
    """Each sparkline canvas sits in a fixed-height positioned box.

    The charts run with ``maintainAspectRatio: false``, so Chart.js sizes
    each canvas to its offset parent. With no wrapper the canvas sized
    itself against the grid cell whose height it was also determining, and
    the loop grew the chart until it filled the viewport.
    """
    markup = _markup(PAGES / "dashboard.html")
    canvases = markup.count("<canvas")
    wrapped = markup.count('<div class="chart-canvas"><canvas')
    assert canvases == wrapped == 4, (
        f"expected 4 canvases each wrapped in .chart-canvas; found {canvases} canvases, "
        f"{wrapped} wrapped"
    )
    css = (
        Path(__file__).resolve().parent.parent
        / "src"
        / "secantus"
        / "admin"
        / "static"
        / "css"
        / "admin.css"
    ).read_text(encoding="utf-8")
    assert ".chart-canvas {" in css, ".chart-canvas rule missing from admin.css"
