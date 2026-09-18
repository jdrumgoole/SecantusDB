"""php-library's text-index failure is a declared gap, not an unexplained one.

`IndexInfoFunctionalTest::testIsText` fails with the server's own
`text indexes are not supported by SecantusDB`. Text indexes are permanently
out of scope (CLAUDE.md), and the SAME gap was already declared for the node
and pymongo gauges — php-library was the one left reading as an unexplained
failure, which is what made its report look like it had something to chase.

The test that matters here is the SECOND one. An entry in
`expected_failures.py` does nothing unless a generator reads it, and php-lib's
did not: it imported only `pass_rate`. A registry entry in
`validation_summary/generate.py` would have been inert too, because that
summary counts only the server-touching categories and `Model` is not one of
them. Both look correct in a diff and change no output.
"""

from __future__ import annotations

import pathlib
import subprocess
import sys

from validation_summary.expected_failures import PHP_LIB, find_match

REPO = pathlib.Path(__file__).resolve().parent.parent

_JUNIT = """<testsuites><testsuite name="s" time="1.0">
  <testcase class="MongoDB\\Tests\\Model\\IndexInfoFunctionalTest" name="testIsText">
    <failure>text indexes are not supported by SecantusDB</failure>
  </testcase>
  <testcase class="MongoDB\\Tests\\Operation\\WhateverTest" name="testOk" />
</testsuite></testsuites>"""


def test_the_text_index_failure_is_declared() -> None:
    assert find_match(PHP_LIB, "MongoDB\\Tests\\Model\\IndexInfoFunctionalTest::testIsText")


def test_a_genuine_failure_is_not_swallowed() -> None:
    """The list must not match anything else -- it declares ONE gap."""
    assert find_match(PHP_LIB, "MongoDB\\Tests\\Operation\\InsertOneTest::testInsert") is None


def test_the_generator_actually_applies_it(tmp_path: pathlib.Path) -> None:
    """End-to-end, because an unread entry is the failure mode here.

    Asserting `find_match` alone would pass just as happily with the generator
    never importing the list -- which is exactly the state this change found.
    """
    xml = tmp_path / "php-lib-junit.xml"
    xml.write_text(_JUNIT)
    out = tmp_path / "report.md"
    subprocess.run(
        [sys.executable, "-m", "php_lib_validation.generate_report", str(xml), str(out)],
        cwd=REPO,
        check=True,
        capture_output=True,
    )
    text = out.read_text()

    overall = next(line for line in text.splitlines() if "**Overall**" in line)
    # passed=1, failed=0, expected=1 -- the failure is RECLASSIFIED, not dropped.
    assert "| **1** | **0** | **1** |" in overall, overall
    # Plain rate still shows it (1 of 2 ran); adjusted removes it.
    assert "**50.0%**" in overall and "**100.0%**" in overall, overall
    assert "## Expected failures (1)" in text
    assert "text indexes" in text
