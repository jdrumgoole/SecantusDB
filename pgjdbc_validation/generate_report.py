"""Turn the pgjdbc gauge's aggregated JUnit results into
docs/validation-report-pgjdbc.md.

Usage:
    python -m pgjdbc_validation.generate_report <raw.json> <output.md>
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
from pathlib import Path


def _secantus_version() -> str:
    try:
        import secantus

        return secantus.__version__
    except ImportError:
        init = Path(__file__).resolve().parent.parent / "src" / "secantus" / "__init__.py"
        m = re.search(r'__version__ = "([^"]+)"', init.read_text())
        return m.group(1) if m else "unknown"


def _pgjdbc_version() -> str:
    head_file = Path("vendor/pgjdbc/.git")
    try:
        if head_file.is_file():
            gitdir = head_file.read_text().split(": ", 1)[1].strip()
            gdir = Path(gitdir) if Path(gitdir).is_absolute() else head_file.parent / gitdir
            return (gdir / "HEAD").read_text().strip()[:12]
        return (head_file / "HEAD").read_text().strip()[:12]
    except OSError:
        return "unknown"


def _merge_raw(paths: list[str]) -> dict:
    """Merge one unsharded raw file, or a COMPLETE shard set, into one payload.

    The same publish discipline as the truncation guard: a partial shard set
    renders a plausible pass rate measured over less of the suite, so a
    missing / duplicate / mismatched shard refuses outright rather than
    shipping a short denominator."""
    datas = [json.loads(Path(p).read_text()) for p in paths]
    for p, d in zip(paths, datas, strict=True):
        if d.get("truncated"):
            # A truncated run's per-class numbers are all correct; only the
            # set of classes is short. That renders a healthy-looking pass
            # rate measured over less of the suite — the most misleading
            # artifact this tool can produce, so refuse rather than publish
            # it with a caveat nobody reads.
            raise SystemExit(
                f"refusing to render a report from a truncated run ({p}): "
                "gradle was killed before finishing, so the pass rate would "
                "be computed over only the classes that happened to complete. "
                "Re-run with a larger SECANTUS_PGJDBC_TIMEOUT."
            )
    shards = [d.get("shard") for d in datas]
    if len(datas) == 1 and shards[0] is None:
        return datas[0]
    if any(s is None for s in shards):
        raise SystemExit(
            "refusing to merge a mix of sharded and unsharded raw files: "
            f"{paths} — pass either one unsharded file or one complete shard set"
        )
    of = {s["of"] for s in shards}
    indexes = sorted(s["index"] for s in shards)
    if len(of) != 1 or indexes != list(range(1, of.pop() + 1)):
        raise SystemExit(
            f"refusing to render a report from an incomplete shard set "
            f"(got indexes {indexes} of {sorted(of) or [s['of'] for s in shards]}): every "
            "shard must be present exactly once or the pass rate is computed "
            "over only part of the suite."
        )
    return {"classes": [c for d in datas for c in d.get("classes", [])]}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "raw_json",
        nargs="+",
        help="one unsharded pgjdbc-raw.json, or every pgjdbc-raw-shard-*.json of a run",
    )
    parser.add_argument("output_md")
    args = parser.parse_args()

    data = _merge_raw(args.raw_json)
    classes = data.get("classes", [])
    totals = {"tests": 0, "failures": 0, "skipped": 0}
    failures: list[str] = []
    lines = [
        "# pgjdbc conformance report",
        "",
        f"- SecantusDB (Python server) {_secantus_version()}",
        f"- suite: vendor/pgjdbc @ {_pgjdbc_version()} (Gradle `:postgresql:test`, unmodified; "
        "60s JUnit default timeout injected)",
        f"- generated: {dt.datetime.now(dt.timezone.utc).strftime('%Y-%m-%d %H:%M UTC')}",
        "",
        "| test class | passed | failed | skipped | total | pass rate |",
        "|---|---|---|---|---|---|",
    ]
    for c in sorted(classes, key=lambda c: c["class"]):
        for k in totals:
            totals[k] += c[k] if k != "failures" else c["failures"]
        passed = c["tests"] - c["failures"] - c["skipped"]
        run = passed + c["failures"]
        rate = f"{passed / run * 100:.1f}%" if run else "—"
        short = c["class"].removeprefix("org.postgresql.test.")
        lines.append(
            f"| {short} | {passed} | {c['failures']} | {c['skipped']} | {c['tests']} | {rate} |"
        )
        failures += [f"{short} :: {t}" for t in c["failed_tests"]]
    passed = totals["tests"] - totals["failures"] - totals["skipped"]
    run = passed + totals["failures"]
    rate = f"{passed / run * 100:.1f}%" if run else "—"
    lines.append(
        f"| **total** | **{passed}** | **{totals['failures']}** | **{totals['skipped']}** "
        f"| **{totals['tests']}** | **{rate}** |"
    )
    if failures:
        lines += ["", f"## Failures ({len(failures)})", ""]
        lines += [f"- `{f}`" for f in sorted(failures)]

    Path(args.output_md).write_text("\n".join(lines) + "\n")
    print(
        f"pgjdbc gauge: {passed} passed / {totals['failures']} failed "
        f"/ {totals['skipped']} skipped ({rate})"
    )


if __name__ == "__main__":
    main()
