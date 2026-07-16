"""mongo-c-driver (libmongoc) conformance gauge runner."""

from __future__ import annotations

import re
from pathlib import Path

# ``test-libmongoc -F`` emits JSON-ish output that is NOT strictly valid:
# every per-test object is followed by a trailing comma (so the last element
# before ``]`` has a dangling comma), and a hard-aborted run leaves the
# ``results`` array unterminated. Rather than repair the document, extract
# each ``{"status": ..., "test_file": ...}`` pair directly — robust against
# both the trailing commas and a truncated tail. Status / test_file is all the
# report and the cross-driver collector need.
_RESULT_RE = re.compile(
    r'"status"\s*:\s*"(?P<status>pass|fail|skip)"\s*,\s*"test_file"\s*:\s*"(?P<name>[^"]*)"'
)


def load_results(path: str | Path) -> dict[str, list[dict[str, str]]]:
    """Parse a ``test-libmongoc -F`` JSON file into ``{"results": [...]}``.

    Tolerant of libmongoc's trailing commas and of a truncated file left
    behind by a hard-aborted run.
    """
    text = Path(path).read_text()
    results = [
        {"status": m.group("status"), "test_file": m.group("name")}
        for m in _RESULT_RE.finditer(text)
    ]
    return {"results": results}
