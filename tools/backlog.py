"""The backlog's items, their IDs, and the checks that verify them.

Each open item in a Rust server's backlog (`tasks/backlog-rust-*.md`) carries
two lines under its headline::

    - [ ] **The headline.** What is missing, how to reproduce it.
          Backlog-ID: RM-012
          Verify: tests/backlog/test_rm_explain.py::test_or_branch_index

``Verify:`` names one or more pytest node ids, comma separated -- GAP TESTS
under ``tests/backlog/``, each marked ``@pytest.mark.backlog("RM-012")``,
that assert what the real product does and so FAIL while the gap is open --
or ``none -- <why no test can check it>`` (a performance budget, a design
note, a CI behaviour).

Usage::

    python tools/backlog.py ids              # give every open item an ID
    python tools/backlog.py list [--file F]  # ID, verify, headline
    python tools/backlog.py verify [--file F] [--id ID ...]

``verify`` runs every named gap test in one pytest run and reports each item:

``OPEN``       every gap test failed on an assertion: the gap reproduces.
``FIXED?``     a gap test passed: the behaviour is right now, so the item is
               probably done -- confirm, then delete it (see the backlog skill).
``BROKEN``     a gap test errored, was skipped, or failed on something other
               than an assertion: the check is not measuring the gap. Fix it.
``UNCHECKED``  the item has ``Verify: none -- ...`` (by design) or no Verify
               line at all (a defect in the backlog: write one).

A fixed item's gap test stays as a REGRESSION test, re-marked
``@pytest.mark.backlog_fixed("RM-012")`` (the item itself is deleted, and the
``changelog.d/`` fragment names both). An unscoped ``verify`` runs every one
of those too and reports any that fail as ``REGRESSED``.

The exit status is 0 only when every checked item is OPEN and no regression
test fails.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field

REPO = pathlib.Path(__file__).resolve().parents[1]
TASKS = REPO / "tasks"
# The files whose items carry IDs and checks, and each one's ID prefix. The
# Python servers' files are deprecated records and are not verified.
FILES = {"backlog-rust-mongodb.md": "RM", "backlog-rust-postgres.md": "RP"}

ITEM = re.compile(r"^(?P<indent>\s*)- \[ \] (?P<head>.*)$")
ID_LINE = re.compile(r"^\s*Backlog-ID:\s*(?P<id>R[MP]-\d{3,})\s*$")
VERIFY_LINE = re.compile(r"^\s*Verify:\s*(?P<what>.*\S)\s*$")
HEADING = re.compile(r"^#{1,4} ")


@dataclass
class Item:
    file: str
    line: int  # 1-based line of the `- [ ]` headline
    indent: int
    headline: str
    id: str | None = None
    verify: str | None = None
    id_line: int | None = None
    body_end: int = 0  # 0-based index one past the item's last line

    @property
    def tests(self) -> list[str]:
        if not self.verify or self.verify.lower().startswith("none"):
            return []
        return [t.strip() for t in self.verify.split(",") if t.strip()]


@dataclass
class Verdict:
    item: Item
    status: str
    detail: list[str] = field(default_factory=list)


def parse(name: str) -> list[Item]:
    """Every open item in one backlog file, with its ID and Verify lines."""
    lines = (TASKS / name).read_text(encoding="utf-8").splitlines()
    items: list[Item] = []
    in_code = False
    for i, line in enumerate(lines):
        if line.lstrip().startswith("```"):
            in_code = not in_code
        if in_code:
            continue
        m = ITEM.match(line)
        if not m:
            continue
        indent = len(m.group("indent"))
        # The item runs to the next item at the same or a shallower indent,
        # the next heading, or the next line back at its indent or less.
        j = i + 1
        while j < len(lines):
            nxt = lines[j]
            if HEADING.match(nxt):
                break
            if nxt.strip() and len(nxt) - len(nxt.lstrip()) <= indent:
                break
            j += 1
        item = Item(name, i + 1, indent, m.group("head")[:160], body_end=j)
        for k in range(i + 1, j):
            if ITEM.match(lines[k]):
                break  # a nested item's lines are its own
            idm = ID_LINE.match(lines[k])
            if idm:
                item.id, item.id_line = idm.group("id"), k
            vm = VERIFY_LINE.match(lines[k])
            if vm:
                item.verify = vm.group("what")
        items.append(item)
    return items


def assign_ids() -> int:
    """Give every open item without a Backlog-ID the next free one."""
    added = 0
    for name, prefix in FILES.items():
        path = TASKS / name
        items = parse(name)
        used = [int(it.id.split("-")[1]) for it in items if it.id]
        nxt = max(used, default=0) + 1
        lines = path.read_text(encoding="utf-8").splitlines()
        # Number top-down; insert bottom-up so earlier line numbers hold.
        new = []
        for it in sorted((it for it in items if not it.id), key=lambda it: it.line):
            new.append((it, f"{prefix}-{nxt:03d}"))
            nxt += 1
        for it, ident in reversed(new):
            at = own_end(lines, it)
            lines.insert(at, f"{body_pad(lines, it, at)}Backlog-ID: {ident}")
            added += 1
        path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return added


def own_end(lines: list[str], it: Item) -> int:
    """Index just past the item's OWN text: before a nested item, and before
    the blank lines that end it."""
    end = it.line  # 0-based index of the line after the headline
    while end < it.body_end and not ITEM.match(lines[end]):
        end += 1
    while end > it.line and not lines[end - 1].strip():
        end -= 1
    return end


def body_pad(lines: list[str], it: Item, at: int) -> str:
    """The indent of the item's continuation lines (two past its dash if it
    has none)."""
    for k in range(it.line, at):
        line = lines[k]
        if line.strip():
            return line[: len(line) - len(line.lstrip())]
    return " " * (it.indent + 2)


def selected(files: list[str] | None, ids: list[str] | None) -> list[Item]:
    out = []
    for name in files or list(FILES):
        out.extend(it for it in parse(name) if not ids or it.id in ids)
    return out


def verify(items: list[Item], regressions: bool = True) -> tuple[list[Verdict], list[dict]]:
    """Run every gap test the items name, in one pytest run."""
    tests = sorted({t for it in items for t in it.tests})
    results: dict[str, dict] = {}
    if tests or regressions:
        with tempfile.TemporaryDirectory() as tmp:
            report = pathlib.Path(tmp) / "report.json"
            env = dict(os.environ, SECANTUS_BACKLOG_REPORT=str(report))
            marks, paths = "backlog", list(tests)
            if regressions:
                marks, paths = "backlog or backlog_fixed", [*tests, "tests/backlog"]
            cmd = [
                sys.executable, "-m", "pytest", "-q", "-p", "no:cacheprovider",
                "-o", "addopts=", "-n", "auto", "-m", marks, *paths,
            ]  # fmt: skip
            subprocess.run(cmd, cwd=REPO, env=env, check=False)
            if report.exists():
                for line in report.read_text(encoding="utf-8").splitlines():
                    rec = json.loads(line)
                    results[rec["nodeid"]] = rec
    verdicts = []
    for it in items:
        if not it.verify:
            verdicts.append(Verdict(it, "UNCHECKED", ["no Verify line"]))
            continue
        if not it.tests:
            verdicts.append(Verdict(it, "UNCHECKED", [it.verify]))
            continue
        statuses, detail = [], []
        for t in it.tests:
            rec = results.get(t)
            if rec is None:
                statuses.append("BROKEN")
                detail.append(f"{t}: never ran (wrong node id?)")
            elif it.id not in rec.get("ids", []):
                statuses.append("BROKEN")
                detail.append(f"{t}: not marked backlog({it.id!r})")
            elif rec["outcome"] == "passed":
                statuses.append("FIXED?")
                detail.append(f"{t}: passed")
            elif rec["outcome"] == "failed" and rec.get("assertion"):
                statuses.append("OPEN")
            else:
                statuses.append("BROKEN")
                detail.append(f"{t}: {rec['outcome']} {rec.get('message', '')[:160]}")
        for s in ("BROKEN", "FIXED?", "OPEN"):
            if s in statuses:
                verdicts.append(Verdict(it, s, detail))
                break
    regressed = [r for r in results.values() if r.get("fixed_ids") and r["outcome"] != "passed"]
    return verdicts, regressed


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("ids")
    for name in ("list", "verify"):
        p = sub.add_parser(name)
        p.add_argument("--file", action="append", choices=list(FILES))
        p.add_argument("--id", action="append")
    args = ap.parse_args()
    if args.cmd == "ids":
        print(f"assigned {assign_ids()} IDs")
        return 0
    items = selected(args.file, args.id)
    if args.cmd == "list":
        for it in items:
            print(f"{it.id or '-------'}  {it.verify or '(no Verify)'}")
            print(f"         {it.file}:{it.line}  {it.headline[:100]}")
        return 0
    # A run scoped to some items checks only those items' gap tests.
    verdicts, regressed = verify(items, regressions=not (args.file or args.id))
    counts: dict[str, int] = {}
    for v in verdicts:
        counts[v.status] = counts.get(v.status, 0) + 1
        if v.status != "OPEN":
            print(f"{v.status:9} {v.item.id or '-------'} {v.item.file}:{v.item.line}")
            for d in v.detail:
                print(f"            {d}")
    for r in regressed:
        print(f"REGRESSED {','.join(r['fixed_ids'])} {r['nodeid']}: {r['message'][:120]}")
    if regressed:
        counts["REGRESSED"] = len(regressed)
    print("=== " + ", ".join(f"{n} {s}" for s, n in sorted(counts.items())))
    checked = [v for v in verdicts if v.status != "UNCHECKED" or v.item.verify is None]
    return 0 if all(v.status == "OPEN" for v in checked) and not regressed else 1


if __name__ == "__main__":
    sys.exit(main())
