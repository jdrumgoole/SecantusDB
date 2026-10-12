"""The Rust MongoDB server decodes BSON literally.

``bson::Document::from_reader`` and ``bson::from_slice`` go through serde, and
the ``bson`` crate's serde visitor reads a sub-document that looks like
extended JSON as the value it would stand for: ``{a: {$numberLong: "1"}}``
comes back as ``{a: Int64(1)}``. A server that decoded a stored document that
way and wrote it back replaced the sub-document, on an update of any other
field (found 2026-10-12 against mongod 8.2.11, which keeps it).

``secantus_core::bsondoc::from_bytes`` is the decoder to use. This fails when
one of the serde decoders reappears in the server's own sources.
"""

from __future__ import annotations

import re
from pathlib import Path

CRATES = Path(__file__).resolve().parent.parent / "crates"

#: The crates a request or a stored document passes through.
SERVER_CRATES = (
    "secantus-core",
    "secantus-commands",
    "secantus-storage",
    "secantus-storage-adapter",
    "secantus-storage-py",
    "secantus-server",
    "secantusdb",
)

SERDE_DECODE = re.compile(r"Document::from_reader\(|bson::from_slice\b|from_slice::<")


def test_no_serde_bson_decode_in_the_server_sources() -> None:
    found = []
    for crate in SERVER_CRATES:
        for path in sorted((CRATES / crate / "src").rglob("*.rs")):
            for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
                if line.lstrip().startswith("//"):
                    continue
                if SERDE_DECODE.search(line):
                    found.append(f"{path.relative_to(CRATES)}:{number}: {line.strip()}")
    assert not found, "decode with secantus_core::bsondoc::from_bytes instead:\n" + "\n".join(found)
