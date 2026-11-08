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
                                             # (each reserved on origin first)
    python tools/backlog.py list [--file F]  # ID, verify, headline
    python tools/backlog.py verify [--file F] [--id ID ...]
    python tools/backlog.py verify --file F --regressions   # what CI runs

``verify`` runs every named gap test in one pytest run and reports each item:

``OPEN``       every gap test failed on an assertion: the gap reproduces.
``FIXED?``     a gap test passed: the behaviour is right now, so the item is
               probably done -- confirm, then delete it (see the backlog skill).
``BROKEN``     a gap test errored, was skipped, or failed on something other
               than an assertion: the check is not measuring the gap. Fix it.
``SKIPPED``    ``--skip ID`` named it, so its gap tests did not run; never
               counted as passing (the caller says why).
``ELSEWHERE``  a gap test skipped itself as ``platform: ...``: mongod's own
               answer differs by platform, and on this machine's platform
               this server already gives it (Joe, 2026-11-07: each
               platform's mongod is the reference). The gap is checked on the
               platform it names, in CI. ``tests/backlog/conftest.py``'s
               ``only_on`` is the one way to say it; a regression test that
               skips the same way is not ``REGRESSED``.
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
import secrets
import socket
import subprocess
import sys
import tempfile
from collections.abc import Iterable
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


def retired_ids(prefix: str) -> list[int]:
    """The numbers of every `prefix` ID the tests and changelogs mention."""
    root = TASKS.parent
    pattern = re.compile(rf"\b{re.escape(prefix)}-(\d+)\b")
    paths = [*sorted((root / "tests").rglob("*.py")), *sorted((root / "changelog.d").glob("*.md"))]
    paths.append(root / "docs" / "changelog.md")
    found: list[int] = []
    for path in paths:
        if path.is_file():
            found += [int(n) for n in pattern.findall(path.read_text(encoding="utf-8"))]
    return found


#: Where a new ID is claimed: one ref per ID on the remote, each pointing at
#: a parentless commit of its own. Pushing a ref that already exists with
#: another commit is rejected (non-fast-forward), so two checkouts can never
#: both hold one number -- the same guarantee a pushed claim branch gives.
#:
#: Why (2026-11-08): IDs were "one past the highest in this checkout's file".
#: The pg-batch lanes branch from main when a batch starts and the other
#: session files on main, so neither saw the other's new IDs for hours.
#: RP-571 and then RP-606 were each handed out twice within the day, and the
#: second pair reached main, where test_backlog_markers failed for everyone
#: until one was renumbered by hand.
RESERVED = "refs/backlog-ids/"
REMOTE = "origin"


class ReservationError(RuntimeError):
    """An ID could not be reserved for a reason other than another checkout
    holding it: no network, no such remote, no permission to push."""


def _git(
    repo: pathlib.Path, *args: str, stdin: str | None = None
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        input=stdin,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=False,
    )


def pushed_ids(
    prefix: str, name: str, repo: pathlib.Path = REPO, remote: str = REMOTE
) -> list[int]:
    """Every `prefix` number already taken on the remote: the reserved refs,
    and every number in each of its branches' copy of the backlog file
    `name`. The branches cover IDs filed before reservation existed and by a
    checkout still running an older copy of this tool."""
    fetch = _git(repo, "fetch", "--quiet", remote)
    if fetch.returncode:
        raise ReservationError(f"git fetch {remote}: {fetch.stderr.strip()}")
    ls = _git(repo, "ls-remote", remote, f"{RESERVED}{prefix}-*")
    if ls.returncode:
        raise ReservationError(f"git ls-remote {remote}: {ls.stderr.strip()}")
    found = [int(n) for n in re.findall(rf"{re.escape(RESERVED + prefix)}-(\d+)$", ls.stdout, re.M)]
    refs = _git(
        repo, "for-each-ref", "--format=%(refname)", f"refs/remotes/{remote}"
    ).stdout.split()
    if refs:
        blobs = _git(
            repo, "cat-file", "--batch", stdin="".join(f"{r}:tasks/{name}\n" for r in refs)
        )
        found += [int(n) for n in re.findall(rf"\b{re.escape(prefix)}-(\d+)\b", blobs.stdout)]
    return found


def reserve(ident: str, repo: pathlib.Path = REPO, remote: str = REMOTE) -> bool:
    """Claim `ident` on the remote: True if this checkout now holds it, False
    if another checkout already does. Anything else raises."""
    tree = _git(repo, "mktree", stdin="")
    if tree.returncode:
        raise ReservationError(f"git mktree: {tree.stderr.strip()}")
    branch = _git(repo, "branch", "--show-current").stdout.strip() or "(detached)"
    # The nonce makes every claim its own commit. Two claims with the same
    # identity, host, pid, branch and second would otherwise hash alike, and
    # the second push would read "up to date" -- both callers holding it.
    who = f"{socket.gethostname()} pid {os.getpid()} on {branch}, nonce {secrets.token_hex(8)}"
    commit = _git(repo, "commit-tree", tree.stdout.strip(), "-m", f"Reserve {ident}\n\n{who}")
    if commit.returncode:
        raise ReservationError(f"git commit-tree: {commit.stderr.strip()}")
    sha = commit.stdout.strip()
    push = _git(repo, "push", "--quiet", remote, f"{sha}:{RESERVED}{ident}")
    if push.returncode == 0:
        return True
    # Rejected because someone holds it, or failed? Ask who holds it now. A
    # push that landed but whose reply was lost shows our own commit.
    ls = _git(repo, "ls-remote", remote, f"{RESERVED}{ident}")
    if ls.returncode == 0 and ls.stdout.strip():
        return ls.stdout.split()[0] == sha
    raise ReservationError(f"git push {remote} {RESERVED}{ident}: {push.stderr.strip()}")


def assign_ids(repo: pathlib.Path = REPO, remote: str = REMOTE) -> int:
    """Give every open item without a Backlog-ID the next free one, each
    reserved on `remote` before it is written."""
    added = 0
    for name, prefix in FILES.items():
        path = TASKS / name
        items = parse(name)
        missing = sorted((it for it in items if not it.id), key=lambda it: it.line)
        if not missing:
            continue
        used = [int(it.id.split("-")[1]) for it in items if it.id]
        # An ID is permanent and never reused, and a closed item's lives on
        # only in its regression test's marker and the changelogs: counting
        # the open items alone handed RP-029 out a second time.
        used += retired_ids(prefix)
        used += pushed_ids(prefix, name, repo, remote)
        nxt = max(used, default=0) + 1
        lines = path.read_text(encoding="utf-8").splitlines()
        # Number top-down; insert bottom-up so earlier line numbers hold.
        new = []
        for it in missing:
            while not reserve(f"{prefix}-{nxt:03d}", repo, remote):
                nxt += 1
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


#: How a gap or regression test that only means something on another
#: platform says so (``tests/backlog/conftest.py``'s ``only_on``).
PLATFORM_SKIP = "platform:"


def elsewhere(rec: dict) -> bool:
    """The test skipped itself because what it checks is on another platform."""
    return rec["outcome"] == "skipped" and rec.get("message", "").startswith(PLATFORM_SKIP)


def regressions_failed(results: Iterable[dict]) -> list[dict]:
    """The regression (``backlog_fixed``) tests that did not pass, a skip for
    another platform aside."""
    return [
        r for r in results if r.get("fixed_ids") and r["outcome"] != "passed" and not elsewhere(r)
    ]


def verify(
    items: list[Item],
    regressions: bool = True,
    prefixes: list[str] | None = None,
    skip: list[str] | None = None,
) -> tuple[list[Verdict], list[dict]]:
    """Run every gap test the items name, in one pytest run.

    ``prefixes`` (``["RM"]``) limits the regression tests to those IDs, so
    one server's items and regressions run without the other's reference."""
    tests = sorted({t for it in items for t in it.tests})
    results: dict[str, dict] = {}
    if tests or regressions:
        with tempfile.TemporaryDirectory() as tmp:
            report = pathlib.Path(tmp) / "report.json"
            env = dict(os.environ, SECANTUS_BACKLOG_REPORT=str(report))
            if prefixes:
                env["SECANTUS_BACKLOG_PREFIXES"] = ",".join(prefixes)
            if skip:
                # Deselected at collection: a regressions run sweeps all of
                # tests/backlog, so leaving them out of `tests` is not enough.
                env["SECANTUS_BACKLOG_SKIP"] = ",".join(skip)
            marks, paths = "backlog", list(tests)
            if regressions:
                marks, paths = "backlog or backlog_fixed", [*tests, "tests/backlog"]
            cmd = [
                sys.executable, "-m", "pytest", "-q", "-p", "no:cacheprovider",
                "-o", "addopts=",
                "-n", os.environ.get("SECANTUS_BACKLOG_WORKERS", "auto"),
                "-m", marks, *paths,
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
            # A parametrized test runs as `t[case]`: every case counts, and the
            # worst of them decides.
            cases = [r for n, r in results.items() if n == t or n.startswith(t + "[")]
            rec = None
            if cases:
                rank = {"error": 0, "skipped": 1, "failed": 3, "passed": 4}
                cases.sort(key=lambda r: (rank.get(r["outcome"], 0), r.get("assertion", False)))
                rec = cases[0]
                if rec["outcome"] == "failed" and not rec.get("assertion"):
                    pass  # a non-assertion failure: BROKEN below
                elif rec["outcome"] == "failed" and any(
                    r["outcome"] == "failed" and r.get("assertion") for r in cases
                ):
                    # Some case shows the gap; a passing case beside it is a
                    # sub-part already fixed, which the item's test should
                    # not still carry -- report it.
                    passing = [r["nodeid"] for r in cases if r["outcome"] == "passed"]
                    rec = next(r for r in cases if r["outcome"] == "failed")
                    if passing:
                        detail.append(f"{t}: cases already passing: {', '.join(passing)}")
            if rec is None:
                statuses.append("BROKEN")
                detail.append(f"{t}: never ran (wrong node id?)")
            elif it.id not in rec.get("ids", []):
                statuses.append("BROKEN")
                detail.append(f"{t}: not marked backlog({it.id!r})")
            elif elsewhere(rec):
                statuses.append("ELSEWHERE")
                detail.append(f"{t}: {rec['message'][:160]}")
            elif rec["outcome"] == "passed":
                statuses.append("FIXED?")
                detail.append(f"{t}: passed")
            elif rec["outcome"] == "failed" and rec.get("assertion"):
                statuses.append("OPEN")
            else:
                statuses.append("BROKEN")
                detail.append(f"{t}: {rec['outcome']} {rec.get('message', '')[:160]}")
        for s in ("BROKEN", "FIXED?", "OPEN", "ELSEWHERE"):
            if s in statuses:
                verdicts.append(Verdict(it, s, detail))
                break
    return verdicts, regressions_failed(results.values())


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("ids")
    for name in ("list", "verify"):
        p = sub.add_parser(name)
        p.add_argument("--file", action="append", choices=list(FILES))
        p.add_argument("--id", action="append")
    sub.choices["verify"].add_argument(
        "--regressions",
        action="store_true",
        help="with --file: also run that file's regression (backlog_fixed) tests",
    )
    sub.choices["verify"].add_argument(
        "--skip",
        action="append",
        default=[],
        metavar="ID",
        help="do not run this item's gap tests; reported SKIPPED, never as passing "
        "(the caller states why, e.g. a test that needs more memory than it has)",
    )
    args = ap.parse_args()
    if args.cmd == "ids":
        try:
            print(f"assigned {assign_ids()} IDs")
        except ReservationError as exc:
            # No unreserved ID is ever written: that is how duplicates happen.
            print(f"backlog.py ids: could not reserve an ID: {exc}", file=sys.stderr)
            return 1
        return 0
    items = selected(args.file, args.id)
    if args.cmd == "list":
        for it in items:
            print(f"{it.id or '-------'}  {it.verify or '(no Verify)'}")
            print(f"         {it.file}:{it.line}  {it.headline[:100]}")
        return 0
    # A run scoped to some items checks only those items' gap tests.
    # --regressions brings them back for a --file run, limited to its prefix
    # (CI runs `verify --file backlog-rust-mongodb.md --regressions`).
    scoped = bool(args.file or args.id)
    prefixes = [FILES[f] for f in args.file] if args.file else None
    skipped = [it for it in items if it.id in args.skip]
    unknown = sorted(set(args.skip) - {it.id for it in items})
    if unknown:
        ap.error(f"--skip names no item in this selection: {', '.join(unknown)}")
    verdicts, regressed = verify(
        [it for it in items if it.id not in args.skip],
        regressions=not scoped or args.regressions,
        prefixes=prefixes,
        skip=args.skip,
    )
    verdicts += [Verdict(it, "SKIPPED", ["--skip: its gap tests were not run"]) for it in skipped]
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
    checked = [
        v
        for v in verdicts
        if v.status != "SKIPPED" and (v.status != "UNCHECKED" or v.item.verify is None)
    ]
    return 0 if all(v.status in ("OPEN", "ELSEWHERE") for v in checked) and not regressed else 1


if __name__ == "__main__":
    sys.exit(main())
