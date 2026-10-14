"""The backlog files' markers must not contradict their own headlines.

The backlog is four files, one per server (`tasks/backlog-*.md`; the
single `tasks/backlog.md` they were split from on 2026-10-14 is now their
index).

The backlog is load-bearing — CLAUDE.md calls it "the only honest record of where
SecantusDB's behaviour diverges from real MongoDB" — and it rotted badly: an audit
on 2026-08-20 found **107 of 178** open-marked items were finished work whose box
was never ticked. A session scanning for `- [ ]` could not tell the handful of real
tasks from the archive, and several entries described work as missing when it had
shipped months earlier, which is how the same thing gets built twice.

This test pins the convention the file's own header states, so the rot cannot
silently come back:

* `- [ ]` is open work, and every item is open: a finished item is deleted
  (git history and `changelog.d/` hold it), not ticked.
* An open item's headline may still *mention* completed context — most
  real items do, e.g. "X landed, Y still missing" — but it must then lead with
  `OPEN —` so a reader scanning headlines is not told the item is done.
"""

from __future__ import annotations

import re
from pathlib import Path

TASKS = Path(__file__).resolve().parent.parent / "tasks"
BACKLOGS = [
    TASKS / f"backlog-{server}.md"
    for server in ("rust-mongodb", "rust-postgres", "python-mongodb", "python-postgres")
]

# An item's headline: the bold title plus roughly the first sentence after it.
HEADLINE_CHARS = 260

OPEN_ITEM = re.compile(r"^\s*- \[ \] (?P<body>.*)$")

# Words that, in a headline, tell a reader the work is finished.
DONE_WORDS = re.compile(
    r"\b(SHIPPED|DONE|FIXED|RESOLVED|landed|COMPLETE|NOW RUNNING)\b", re.IGNORECASE
)

# The explicit escape hatch: an open item whose headline leads with OPEN is
# unambiguous no matter what context follows.
LEADS_WITH_OPEN = re.compile(r"^\*\*OPEN\b", re.IGNORECASE)


def open_items(backlog: Path) -> list[tuple[int, str]]:
    """(line number, headline) for every `- [ ]` item in one backlog file."""
    items = []
    # Explicit UTF-8: the backlog is full of em dashes and "×", and Windows
    # defaults `read_text()` to cp1252, which dies on them with a
    # UnicodeDecodeError. Caught by the Windows CI lane, not locally.
    for lineno, line in enumerate(backlog.read_text(encoding="utf-8").splitlines(), 1):
        m = OPEN_ITEM.match(line)
        if m:
            items.append((lineno, m.group("body")[:HEADLINE_CHARS]))
    return items


def test_backlogs_exist_and_have_open_items() -> None:
    """Guard against the parser silently matching nothing."""
    for backlog in BACKLOGS:
        assert backlog.is_file(), f"{backlog} missing"
        assert open_items(backlog), f"no open items parsed in {backlog.name}"


def test_no_item_is_ticked() -> None:
    """A finished item is deleted, not ticked: the files hold open work."""
    ticked = [
        f"{backlog.name}:{lineno}"
        for backlog in BACKLOGS
        for lineno, line in enumerate(backlog.read_text(encoding="utf-8").splitlines(), 1)
        if re.match(r"^\s*- \[x\] ", line)
    ]
    assert not ticked, "ticked items in the backlog (delete them instead): " + ", ".join(ticked)


def test_no_open_item_claims_to_be_finished() -> None:
    """An item marked open must not read as finished.

    Fix an offender one of two ways: tick the box if the work is done, or lead the
    bold title with `OPEN —` if completed context genuinely belongs in the
    headline ("OPEN — jsonb operator surface landed (one gap …)").
    """
    offenders = [
        (backlog.name, lineno, headline)
        for backlog in BACKLOGS
        for lineno, headline in open_items(backlog)
        if DONE_WORDS.search(headline) and not LEADS_WITH_OPEN.match(headline.strip())
    ]
    assert not offenders, (
        "open-marked backlog items whose headline claims completion:\n"
        + "\n".join(
            f"  tasks/{name}:{lineno}  {headline[:110]}" for name, lineno, headline in offenders
        )
    )
