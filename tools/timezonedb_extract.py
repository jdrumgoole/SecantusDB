"""Extract mongod's built-in time-zone database for the Rust MongoDB server.

mongod does not read the system's zoneinfo: it compiles in timelib's
`timezonedb.h`, a generated C file holding every zone as TZif data with
timelib's own preamble. The Rust server answers time-zone questions from the
SAME data (RM-249; decided by Joe, 2026-10-22), so this script copies it out of
mongod's source at a release tag, byte for byte:

    crates/secantus-core/tzdata/timezonedb.bin   the zone data, as compiled
    crates/secantus-core/tzdata/timezonedb.idx   "<version>" then one
                                                 "<name>\\t<offset>" per zone

Only the "fat" half of the header is taken: mongod's Bazel build does not
define TIMELIB_SUPPORT_SLIM_FILE.

    python tools/timezonedb_extract.py --ref r8.2.11
    python tools/timezonedb_extract.py --header path/to/timezonedb.h --check
"""

from __future__ import annotations

import argparse
import re
import sys
import urllib.request
from pathlib import Path

URL = "https://raw.githubusercontent.com/mongodb/mongo/{ref}/src/third_party/timelib/dist/zones/timezonedb.h"
OUT = Path(__file__).resolve().parent.parent / "crates" / "secantus-core" / "tzdata"


def extract(header: str) -> tuple[str, list[tuple[str, int]], bytes]:
    """The database version, the index and the data of the fat half."""
    fat = header[header.index("#else") : header.index("#endif", header.index("#else"))]
    index = [
        (name, int(pos, 16))
        for name, pos in re.findall(r'\{ \(char\*\) "([^"]+)"\s*, (0x[0-9A-F]+) \}', fat)
    ]
    declared = re.search(r"timelib_timezone_db_data_builtin\[(\d+)\]", fat)
    body = fat[declared.end() :]
    data = bytes(int(b, 16) for b in re.findall(r"0x([0-9A-Fa-f]{2})\b", body))
    version = re.search(r'timezonedb_builtin = \{ "([^"]+)", (\d+),', header)
    if version is None or declared is None:
        raise SystemExit("not a timelib timezonedb.h")
    if len(index) != int(version.group(2)) or len(data) != int(declared.group(1)):
        raise SystemExit(f"parse mismatch: {len(index)} zones, {len(data)} bytes")
    if any(not data[pos:].startswith(b"PHP2") for _, pos in index):
        raise SystemExit("an index entry does not point at a PHP2 preamble")
    return version.group(1), index, data


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    src = ap.add_mutually_exclusive_group(required=True)
    src.add_argument("--ref", help="a mongodb/mongo git ref, e.g. r8.2.11")
    src.add_argument("--header", type=Path, help="a local timezonedb.h")
    ap.add_argument(
        "--check", action="store_true", help="compare with the committed files; write nothing"
    )
    args = ap.parse_args()
    if args.ref:
        with urllib.request.urlopen(URL.format(ref=args.ref)) as r:  # noqa: S310 -- a fixed https URL
            header = r.read().decode()
    else:
        header = args.header.read_text()
    version, index, data = extract(header)
    idx = version + "\n" + "".join(f"{name}\t{pos}\n" for name, pos in index)
    if args.check:
        same = (OUT / "timezonedb.bin").read_bytes() == data and (
            OUT / "timezonedb.idx"
        ).read_text() == idx
        print(
            "identical" if same else "DIFFERENT",
            f"(timezonedb {version}, {len(index)} zones, {len(data)} bytes)",
        )
        return 0 if same else 1
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "timezonedb.bin").write_bytes(data)
    (OUT / "timezonedb.idx").write_text(idx)
    print(f"wrote timezonedb {version}: {len(index)} zones, {len(data)} bytes")
    return 0


if __name__ == "__main__":
    sys.exit(main())
