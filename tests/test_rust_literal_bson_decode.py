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

#: Every crate of the MongoDB side: the ones a request or a stored document
#: passes through, the auth and wire crates, and the Python bindings,
#: `secantus-core-py` (the parity binding) and `secantus-server-py`
#: (decided 2026-10-16, RM-058). The PostgreSQL crates have a check of
#: their own.
SERVER_CRATES = (
    "secantus-auth",
    "secantus-core",
    "secantus-core-py",
    "secantus-commands",
    "secantus-storage",
    "secantus-storage-adapter",
    "secantus-storage-py",
    "secantus-server",
    "secantus-server-py",
    "secantus-wire",
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


#: The `bson` crate's own encoders. A string whose bytes are not UTF-8 is
#: held in a `Document` as a carrier that only `bsondoc::write` / `to_vec` /
#: `to_raw` turn back into its bytes (RM-222); the crate's encoders refuse
#: it, so a reply or a stored document written through one of them fails for
#: such a string where mongod answers.
CRATE_ENCODE = re.compile(
    r"\.to_writer\(|bson::to_vec\b|RawDocumentBuf::from_document\b|to_raw_document_buf\b"
)


def _non_test_lines(path: Path):
    lines = path.read_text(encoding="utf-8").splitlines()
    for number, line in enumerate(lines, start=1):
        # Unit tests sit in a trailing `#[cfg(test)] mod`; they may build
        # their fixtures however they like.
        if line.strip() == "#[cfg(test)]" and number < len(lines) and "mod " in lines[number]:
            return
        if not line.lstrip().startswith("//"):
            yield number, line


def test_no_crate_bson_encode_in_the_server_sources() -> None:
    found = []
    for crate in SERVER_CRATES:
        for path in sorted((CRATES / crate / "src").rglob("*.rs")):
            if path.name == "bsondoc.rs":
                continue
            for number, line in _non_test_lines(path):
                if CRATE_ENCODE.search(line):
                    found.append(f"{path.relative_to(CRATES)}:{number}: {line.strip()}")
    assert not found, "encode with secantus_core::bsondoc::write / to_vec instead:\n" + "\n".join(
        found
    )
