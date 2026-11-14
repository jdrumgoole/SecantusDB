"""`getCmdLineOpts`, measured against mongod 8.2.1 (2026-11-08).

mongod answers with the command line it was started with: `argv` as given,
and `parsed` holding each option given, under its configuration-file path
(`--port 27031 --dbpath D` is `{net: {port: 27031}, storage: {dbPath: D}}`).
The same `parsed` is the `cmdLine` of each `local.startup_log` record.
"""

from __future__ import annotations

import pathlib
import subprocess

import pymongo
import pytest

from tests.backlog.conftest import _RS_BANNER, RS_BINARY, drain_output


@pytest.mark.backlog_fixed("RM-425")
def test_get_cmd_line_opts_reports_the_command_line(tmp_path: pathlib.Path) -> None:
    """`parsed` named each option it was given, and `argv` was the
    command line itself."""
    store = tmp_path / "store"
    argv = [str(RS_BINARY), "--port", "0", "--storage-path", str(store)]
    proc = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    try:
        assert proc.stdout is not None
        line, m = "", None
        for _ in range(20):
            line = proc.stdout.readline()
            m = _RS_BANNER.search(line)
            if m or not line:
                break
        assert m, f"secantusd-rs did not start: {line!r}"
        drain_output(proc)
        client = pymongo.MongoClient(
            host=m.group(1), port=int(m.group(2)), serverSelectionTimeoutMS=10_000
        )
        try:
            got = client.admin.command("getCmdLineOpts")
        finally:
            client.close()
    finally:
        proc.terminate()
        proc.wait(timeout=20)
    assert {"argv": got["argv"], "parsed": got["parsed"]} == {
        "argv": argv,
        "parsed": {"net": {"port": 0}, "storage": {"dbPath": str(store)}},
    }
