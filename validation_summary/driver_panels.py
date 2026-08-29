"""Regenerate the secantusdb.com driver-panel grid from validation data.

The marketing site's ``themes/secantus/templates/partials/drivers_grid.html``
shows one panel per driver gauge: headline pass-rate + tests passed /
failed counts + a short prose note + a link to the per-driver
validation report. The numbers must match what shipped — until now
they were hand-edited and drifted release to release.

This module:

- Reads the same ``.validation/`` raw artifacts the cross-driver
  summary parses (via ``validation_summary.generate`` collectors).
- Pulls the prose notes / labels / report URLs from the curated
  ``PANEL_PROSE`` / ``SMOKE_PANELS`` constants below — those stay
  human-edited because they describe *what we test*, not the numbers.
- Emits the complete ``drivers_grid.html`` ready to drop into the
  website worktree.

Usage::

    uv run --no-sync python -m validation_summary.driver_panels \\
        --raw-dir .validation \\
        --out ../SecantusDB-website/website/themes/secantus/\\
              templates/partials/drivers_grid.html

Run after ``invoke validate-all`` (which populates ``.validation/``)
and before deploying the website — wired into the release pipeline
via the secantusdb-release skill.
"""

from __future__ import annotations

import argparse
import sys
from html import escape
from pathlib import Path

from validation_summary.generate import (
    GaugeStats,
    _apply_expected_failures,
    _collect_c,
    _collect_cxx,
    _collect_dotnet,
    _collect_go,
    _collect_java,
    _collect_kotlin,
    _collect_node,
    _collect_php_ext,
    _collect_php_lib,
    _collect_pymongo,
    _collect_pymongo_async,
    _collect_ruby,
    _collect_rust,
)

# Prose notes per validated driver. The numbers come from the
# validation artifacts; only this dict is hand-curated. Order is the
# display order on the home-page grid.
PANEL_PROSE: dict[str, dict[str, str]] = {
    "pymongo": {
        "title": "pymongo",
        "lang": "Python",
        "note": (
            "The official MongoDB Python driver, and the deepest suite we "
            "run &mdash; so it surfaces the long tail. The remaining "
            "failures are features outside a single node's scope (text / "
            "hashed indexes, server-side <code>$where</code> JavaScript), "
            "tests that assume a multi-node cluster, and a few driver-side "
            "harness artifacts &mdash; not gaps in the CRUD, aggregation, or "
            "change-stream surface that test and dev rely on. We run "
            "pymongo's own tests, unmodified, against an embedded SecantusDB."
        ),
        "report_url": ("https://secantusdb.com/docs/validation-report.html"),
    },
    "pymongo (async)": {
        "title": "pymongo (async)",
        "lang": "Python / asyncio",
        "note": (
            "pymongo's native <code>AsyncMongoClient</code> suite &mdash; the "
            "async/await wire path that replaced Motor. Same unmodified "
            "upstream tests as the sync gauge, run under "
            "<code>pytest-asyncio</code> against the same embedded "
            "SecantusDB, so the non-blocking client is held to the same bar "
            "rather than assumed to follow from the sync one. Remaining "
            "failures are the same out-of-scope surfaces (text / hashed "
            "indexes, server-side <code>$where</code>) plus a few "
            "timeout-introspection tests."
        ),
        "report_url": ("https://secantusdb.com/docs/validation-report-pymongo-async.html"),
    },
    "mongo-java-driver": {
        "title": "mongo-java-driver",
        "lang": "Java",
        "note": (
            "The driver enterprise MongoDB consumers most often use, and the "
            "foundation for many JVM-language wrappers. We run a curated "
            "subset of <code>driver-sync/src/test/functional/</code> &mdash; "
            "integration tests that open a real connection to a SecantusDB "
            "daemon (the driver's own BSON codec unit tests are run but not "
            "counted here; they never touch the server). Type-strict decoders "
            "catch wire-shape divergences pymongo's permissive client accepts "
            "silently."
        ),
        "report_url": ("https://secantusdb.com/docs/validation-report-java.html"),
    },
    "mongo-kotlin-driver": {
        "title": "mongo-kotlin-driver",
        "lang": "Kotlin",
        "note": (
            "The official Kotlin driver, which ships inside the "
            "mongo-java-driver monorepo and is the coroutine-friendly entry "
            "point for JVM services written in Kotlin. We run its "
            "<code>:driver-kotlin-sync:integrationTest</code> suite unmodified "
            "against a standalone SecantusDB daemon, through the same "
            "two-phase auth setup the Java gauge uses."
        ),
        "report_url": ("https://secantusdb.com/docs/validation-report-kotlin.html"),
    },
    "mongo-node-driver": {
        "title": "mongo-node-driver",
        "lang": "Node.js",
        "note": (
            "The official Node driver, and the same driver <code>mongosh</code> "
            "and the JavaScript ecosystem build on. We run a curated "
            "<code>test/integration/</code> spec set via "
            "<code>mocha --config test/mocha_mongodb.js</code> &mdash; real "
            "wire commands against an embedded SecantusDB daemon."
        ),
        "report_url": ("https://secantusdb.com/docs/validation-report-node.html"),
    },
    "mongo-go-driver": {
        "title": "mongo-go-driver",
        "lang": "Go",
        "note": (
            "The same driver <code>mongodump</code> and <code>mongorestore</code> "
            "are built on. We run <code>./internal/integration/...</code> &mdash; "
            "the package that opens real <code>mongo.Client</code> instances "
            "and exchanges wire commands. Type-strict (<code>int32</code> vs "
            "<code>int64</code>) bugs that pymongo accepts silently fail "
            "loudly here."
        ),
        "report_url": ("https://secantusdb.com/docs/validation-report-go.html"),
    },
    "mongo-ruby-driver": {
        "title": "mongo-ruby-driver",
        "lang": "Ruby",
        "note": (
            "The official MongoDB Ruby driver (mongo + bson 5.x), the gem the "
            "Rails / Sinatra ecosystem builds on. We run a curated set of "
            "integration spec files end-to-end against an embedded SecantusDB "
            "daemon &mdash; every test opens a real <code>Mongo::Client</code>, "
            "SCRAM-authenticates as a pre-provisioned <code>root-user</code>, "
            "and exchanges wire commands."
        ),
        "report_url": ("https://secantusdb.com/docs/validation-report-ruby.html"),
    },
    "mongo-rust-driver": {
        "title": "mongo-rust-driver",
        "lang": "Rust",
        "note": (
            "The official MongoDB Rust driver &mdash; the basis for "
            "Tokio-async MongoDB consumers in Rust. We run a curated set of "
            "<code>driver/src/test/</code> in-tree tests via <code>cargo "
            "test --lib -p mongodb</code> with <code>MONGODB_URI</code> "
            "explicitly overridden in the subprocess env, so the rust "
            "driver's fallback chain (<code>$MONGODB_URI</code> &rarr; "
            "<code>~/.mongodb_uri</code> &rarr; <code>localhost:27017</code>) "
            "can't accidentally route to a real mongod."
        ),
        "report_url": ("https://secantusdb.com/docs/validation-report-rust.html"),
    },
    "mongo-php-library": {
        "title": "mongo-php-library",
        "lang": "PHP",
        "note": (
            "The official high-level PHP library &mdash; the "
            "<code>mongodb/mongodb</code> package Laravel and Symfony "
            "applications build on. We run its PHPUnit functional suite "
            "(<code>Operation</code> / <code>Collection</code> / "
            "<code>Database</code> / <code>Command</code>) end-to-end against "
            "an embedded SecantusDB daemon; the pure-code query-builder and "
            "BSON-comparator units are run but not counted here, since they "
            "never touch the server."
        ),
        "report_url": (
            "https://secantusdb.com/docs/validation-report-php-lib.html"
        ),
    },
    "mongo-php-driver": {
        "title": "mongo-php-driver",
        "lang": "PHP",
        "note": (
            "The low-level PHP extension &mdash; the C-based PECL "
            "<code>ext-mongodb</code> that wraps libmongoc and underpins the "
            "library above. We run its <code>.phpt</code> wire-protocol suite "
            "via PHP's <code>run-tests.php</code> against an embedded "
            "SecantusDB daemon (the pure BSON-serialization tests are run but "
            "not counted). Alongside Go, the strictest wire-shape check we "
            "run &mdash; type divergences pymongo accepts silently fail here."
        ),
        "report_url": (
            "https://secantusdb.com/docs/validation-report-php-ext.html"
        ),
    },
    "mongo-c-driver": {
        "title": "mongo-c-driver",
        "lang": "C",
        "note": (
            "The official MongoDB <strong>C</strong> driver (<code>libmongoc</code>) "
            "&mdash; the lowest-level official client, the one the PHP, Ruby (bson), "
            "and PyMongo C-extensions ultimately wrap. We build its "
            "<code>test-libmongoc</code> suite from source and run a curated set of "
            "wire-protocol prefixes (CRUD / cursor / aggregate / command / GridFS / "
            "index management) against an embedded SecantusDB daemon via "
            "<code>MONGOC_TEST_URI</code>. A strict C client &mdash; type and "
            "wire-shape divergences surface here that permissive clients accept."
        ),
        "report_url": ("https://secantusdb.com/docs/validation-report-c.html"),
    },
    "mongo-cxx-driver": {
        "title": "mongo-cxx-driver",
        "lang": "C++",
        "note": (
            "The official MongoDB <strong>C++</strong> driver (<code>mongocxx</code>), "
            "built on libmongoc. We build its Catch2 <code>test_driver</code> suite "
            "from source and run it (CRUD / cursor / aggregate / GridFS / commands) "
            "against an embedded SecantusDB daemon. mongocxx's tests hard-wire the "
            "driver default port, so the gauge serves them on <code>127.0.0.1:27017</code>."
        ),
        "report_url": ("https://secantusdb.com/docs/validation-report-cxx.html"),
    },
    "mongo-csharp-driver": {
        "title": "mongo-csharp-driver",
        "lang": "C#",
        "note": (
            "The official MongoDB <strong>C# / .NET</strong> driver — the one the "
            ".NET / Unity / Xamarin ecosystem builds on. We run its xUnit CRUD "
            "specification suite (<code>MongoDB.Driver.Tests.Specifications.crud</code>) "
            "via <code>dotnet test</code> against an embedded SecantusDB daemon, with "
            "<code>MONGODB_URI</code> pointed at it. The driver's "
            "<code>[RequireServer]</code> attribute self-skips version- and "
            "topology-gated cases."
        ),
        "report_url": (
            "https://secantusdb.com/docs/validation-report-dotnet.html"
        ),
    },
}

# Trailing panels that aren't backed by ``.validation/`` raw data —
# feature-smoke or pending. Kept fully hand-edited. (The PHP library /
# extension graduated from a hand-written smoke panel to real gauges.)
SMOKE_PANELS: list[dict[str, str | None]] = []


_COLLECTORS = {
    "pymongo": _collect_pymongo,
    "pymongo (async)": _collect_pymongo_async,
    "mongo-java-driver": _collect_java,
    "mongo-kotlin-driver": _collect_kotlin,
    "mongo-node-driver": _collect_node,
    "mongo-go-driver": _collect_go,
    "mongo-ruby-driver": _collect_ruby,
    "mongo-rust-driver": _collect_rust,
    "mongo-php-library": _collect_php_lib,
    "mongo-php-driver": _collect_php_ext,
    "mongo-c-driver": _collect_c,
    "mongo-cxx-driver": _collect_cxx,
    "mongo-csharp-driver": _collect_dotnet,
}


def _format_rate(stats: GaugeStats) -> str:
    """Headline pass-rate string used in the panel.

    True raw rate: ``passed / ran``, with **no** expected-failure
    exclusion. Every gauge is scored the same way — a documented
    divergence still counts against the rate, exactly as it does in the
    per-driver validation reports (``validation_summary.generate``) and
    in the pymongo gauge. Excluding "expected" failures from the
    denominator here (while pymongo carries none) made the secondary
    drivers read 100% against pymongo's 99.2% for the same kind of
    documented gap — an apples-to-oranges flatter the panels no longer
    do. The "N known divergence" count label below still names the
    documented failure; the rate just no longer hides it.
    """
    if stats.ran <= 0:
        return "&mdash;"
    pct = stats.passed / stats.ran * 100
    return f"{pct:.1f}%"


def _render_validation_panel(name: str, stats: GaugeStats) -> str:
    prose = PANEL_PROSE[name]
    rate = _format_rate(stats)
    if stats.expected_failures > 0 and stats.actionable_failures == 0:
        # Clean panel with a known, report-documented divergence. Fold it
        # in plainly ("N known divergence") rather than spelling out the
        # rate accounting ("0 unexpected failures · ... excluded from the
        # rate"), which reads defensively on a marketing card — the report
        # carries the detail.
        word = "known divergence" if stats.expected_failures == 1 else "known divergences"
        counts = (
            f"<strong>{stats.passed}</strong> tests passed &middot; "
            f"<strong>{stats.expected_failures}</strong> {word}"
        )
    else:
        counts = (
            f"<strong>{stats.passed}</strong> tests passed &middot; "
            f"<strong>{stats.actionable_failures}</strong> failed"
        )
    return (
        f'  <article class="driver">\n'
        f"    <header>\n"
        f"      <h3>{escape(prose['title'])}</h3>\n"
        f'      <span class="lang">{escape(prose["lang"])}</span>\n'
        f"    </header>\n"
        f'    <div class="pass-rate">\n'
        f'      <span class="rate">{rate}</span>\n'
        f'      <span class="rate-label">pass rate</span>\n'
        f"    </div>\n"
        f'    <p class="counts">{counts}</p>\n'
        f'    <p class="note">{prose["note"]}</p>\n'
        f'    <a class="report" href="{prose["report_url"]}" rel="noopener">'
        f"Read the report &rarr;</a>\n"
        f"  </article>\n"
    )


def _render_smoke_panel(panel: dict[str, str | None]) -> str:
    parts = [
        '  <article class="driver">\n',
        "    <header>\n",
        f"      <h3>{escape(str(panel['title']))}</h3>\n",
        f'      <span class="lang">{escape(str(panel["lang"]))}</span>\n',
    ]
    if panel.get("kind"):
        parts.append(
            f'      <span class="kind {escape(str(panel["kind"]))}">'
            f"{escape(str(panel['kind_label']))}</span>\n"
        )
    parts.append("    </header>\n")
    if panel.get("rate_value"):
        parts.extend(
            [
                '    <div class="pass-rate">\n',
                f'      <span class="rate">{escape(str(panel["rate_value"]))}</span>\n',
                f'      <span class="rate-label">{escape(str(panel["rate_label"]))}</span>\n',
                "    </div>\n",
            ]
        )
    parts.append(f'    <p class="note">{panel["note"]}</p>\n')
    if panel.get("report_url"):
        parts.append(
            f'    <a class="report" href="{panel["report_url"]}" rel="noopener">'
            f"Read the report &rarr;</a>\n"
        )
    parts.append("  </article>\n")
    return "".join(parts)


_GRID_FOOT = """\
<p class="drivers-foot">
  Plus a <strong>cross-driver feature matrix</strong>: every shipped feature
  (auth, change streams, custom roles, sessions, geo, bulk writes, type
  fidelity&hellip;) is verified end-to-end through the Python / Java /
  Node / Go / Ruby / PHP drivers via dedicated smoke tests &mdash; so a
  wire-shape divergence that one driver is permissive about gets caught
  by another.
</p>
"""


def render(raw_dir: Path) -> str:
    """Build the full ``drivers_grid.html`` text from ``.validation/``."""
    panels: list[str] = []
    for name, collector in _COLLECTORS.items():
        stats = collector(raw_dir)
        if stats is None:
            raise SystemExit(
                f"missing validation artifact for {name!r} under {raw_dir}; "
                "run `invoke validate-all` first"
            )
        _apply_expected_failures(stats)
        panels.append(_render_validation_panel(name, stats))
    for s in SMOKE_PANELS:
        panels.append(_render_smoke_panel(s))
    return '<div class="drivers">\n' + "\n".join(panels) + "</div>\n\n" + _GRID_FOOT


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Regenerate secantusdb.com's driver-panel grid HTML from the "
            "current .validation/ artifacts."
        ),
    )
    parser.add_argument(
        "--raw-dir",
        type=Path,
        default=Path(".validation"),
        help="Directory holding the per-gauge raw artifacts (default: .validation).",
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=None,
        help=(
            "Where to write drivers_grid.html (typically "
            "<website-worktree>/website/themes/secantus/templates/"
            "partials/drivers_grid.html). Required unless ``--print`` is set."
        ),
    )
    parser.add_argument(
        "--print",
        dest="just_print",
        action="store_true",
        help="Print the rendered HTML to stdout instead of writing to disk.",
    )
    args = parser.parse_args(argv)

    if not args.just_print and args.out is None:
        parser.error("--out is required unless --print is set")

    html = render(args.raw_dir)
    if args.just_print:
        sys.stdout.write(html)
        return 0
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(html, encoding="utf-8")
    print(f"wrote {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
