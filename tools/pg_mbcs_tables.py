"""Measure PostgreSQL 15's multi-byte encoding conversions and write the
tables the Rust PostgreSQL server converts with
(``crates/secantus-pgplan/src/mbcs_data/``, read by ``mbcs_maps.rs``).

Nothing here is written from a published mapping: every table is what
PostgreSQL itself answers. For each encoding the script asks the reference
server to

* ``convert_from`` every byte sequence the encoding's verifier can accept
  (both bytes of every two-byte form, every three- and four-byte form),
  keeping the ones that convert;
* ``convert_to`` every code point from U+0080 to U+10FFFF, keeping the ones
  that convert -- PostgreSQL's two directions are not always inverses (a
  code point may have no bytes although some bytes decode to it, or bytes
  no sequence decodes to);
* ``convert`` every source sequence of the three pairs PostgreSQL converts
  between directly (EUC_JP / SJIS, EUC_TW / BIG5, EUC_JIS_2004 /
  SHIFT_JIS_2004), where the answer is not the one through Unicode.

Each loop runs server-side in a ``pg_temp`` PL/pgSQL function that catches
the error per sequence, so it touches nothing other sessions can see.

GB18030's four-byte ranges are algorithmic in PostgreSQL
(``utf8_and_gb18030.c``'s ``conv_18030_to_utf8``); ``mbcs.rs`` carries that
rule, and the script checks every measured code in those ranges against it
before leaving them out of the table.

File format (zlib-compressed): three sections, each a varint count followed
by its entries, every number a LEB128 varint:

1. decode: ``code delta, zigzag(code point delta)`` sorted by code -- the
   sequence's bytes read big-endian as one number, and the one code point
   it converts to;
2. pairs: ``code, cp1, cp2`` -- a sequence that converts to two code
   points (EUC_JIS_2004's and SHIFT_JIS_2004's combined characters), which
   ``convert_to`` also turns the pair back into;
3. encode exceptions: ``cp delta, code`` sorted by code point, where the
   default is the lowest code that decodes to the code point and 0 means
   the code point has no bytes.

A direct pair's file is one section, ``code delta, target`` sorted by source
code, for each source whose answer differs from the composition through
Unicode (target 0: refused). A refusal is the pair's own error, `DIRECT`
below, which the script checks against every refusal it measures.

Usage (PostgreSQL 15 on port 5415 unless ``SECANTUS_PG_ORACLE_DSN_15``)::

    python tools/pg_mbcs_tables.py [--only EUC_JP ...] [--dump DIR]

``--dump`` also writes every answer, errors included, as text, for
comparing a server against.
"""

from __future__ import annotations

import argparse
import os
import zlib
from pathlib import Path

import psycopg

OUT = Path(__file__).resolve().parent.parent / "crates/secantus-pgplan/src/mbcs_data"

ENCODINGS = [
    "EUC_JP",
    "EUC_CN",
    "EUC_KR",
    "EUC_TW",
    "SJIS",
    "BIG5",
    "GBK",
    "UHC",
    "GB18030",
    "JOHAB",
    "SHIFT_JIS_2004",
    "EUC_JIS_2004",
]

# The pairs PostgreSQL converts between directly, and the error each
# conversion function reports for a character it cannot convert: most name
# the target (22P05), `euc_jis_20042shift_jis_2004` calls the character
# invalid (22021).
DIRECT = {
    ("EUC_JP", "SJIS"): "22P05",
    ("SJIS", "EUC_JP"): "22P05",
    ("EUC_TW", "BIG5"): "22P05",
    ("BIG5", "EUC_TW"): "22P05",
    ("EUC_JIS_2004", "SHIFT_JIS_2004"): "22021",
    ("SHIFT_JIS_2004", "EUC_JIS_2004"): "22P05",
}

EUC = range(0xA1, 0xFF)
HIGH = range(0x80, 0x100)


def candidates(enc: str) -> list[bytes]:
    """Every sequence of `enc` its verifier could accept, and a margin."""
    two_any = [bytes([a, b]) for a in HIGH for b in range(1, 0x100)]
    euc2 = [bytes([a, b]) for a in EUC for b in EUC]
    if enc in ("EUC_JP", "EUC_JIS_2004"):
        return (
            euc2 + [bytes([0x8E, b]) for b in EUC] + [bytes([0x8F, a, b]) for a in EUC for b in EUC]
        )
    if enc in ("EUC_CN", "EUC_KR"):
        return euc2
    if enc == "EUC_TW":
        # Its verifier checks only the second byte of a two-byte character.
        return (
            [bytes([a, b]) for a in HIGH if a not in (0x8E, 0x8F) for b in EUC]
            + [bytes([0x8E, p, a, b]) for p in range(0xA1, 0xB1) for a in EUC for b in EUC]
            + [bytes([0x8F, a, b]) for a in EUC for b in EUC]
        )
    if enc in ("SJIS", "SHIFT_JIS_2004"):
        # A byte 0xA1..0xDF is a character on its own (half-width kana).
        leads = [a for a in HIGH if not 0xA1 <= a <= 0xDF]
        return [bytes([a]) for a in HIGH] + [
            bytes([a, b]) for a in leads for b in range(0x40, 0xFD)
        ]
    if enc in ("BIG5", "GBK", "UHC"):
        return two_any
    if enc == "JOHAB":
        return two_any + [bytes([0x8F, a, b]) for a in EUC for b in EUC]
    if enc == "GB18030":
        tails = [*range(0x40, 0x7F), *range(0x80, 0xFF)]
        return [bytes([a, b]) for a in range(0x81, 0xFF) for b in tails] + [
            bytes([a, b, c, d])
            for a in range(0x81, 0xFF)
            for b in range(0x30, 0x3A)
            for c in range(0x81, 0xFF)
            for d in range(0x30, 0x3A)
        ]
    raise ValueError(enc)


# `conv_18030_to_utf8`'s ranges: (first code point, first code, last code).
GB18030_RANGES = [
    (0x0452, 0x8130D330, 0x8136A531),
    (0x2643, 0x8137A839, 0x8138FD38),
    (0x361B, 0x8230A633, 0x8230F237),
    (0x3CE1, 0x8231D438, 0x8232AF32),
    (0x4160, 0x8232C937, 0x8232F837),
    (0x44D7, 0x8233A339, 0x8233C931),
    (0x478E, 0x8233E838, 0x82349638),
    (0x49B8, 0x8234A131, 0x8234E733),
    (0x9FA6, 0x82358F33, 0x8336C738),
    (0xE865, 0x8336D030, 0x84308534),
    (0xFA2A, 0x84309C38, 0x84318730),
    (0x10000, 0x90308130, 0xE3329A35),
]


def gb_linear(code: int) -> int:
    b0, b1, b2, b3 = code >> 24, (code >> 16) & 0xFF, (code >> 8) & 0xFF, code & 0xFF
    return b0 * 12600 + b1 * 1260 + b2 * 10 + b3


def gb18030_rule(code: int) -> int | None:
    for cp, lo, hi in GB18030_RANGES:
        if lo <= code <= hi:
            return gb_linear(code) - gb_linear(lo) + cp
    return None


SETUP = [
    """create function pg_temp.mb_dec(enc text, bs bytea[])
       returns table(t text, st text, m text) language plpgsql as $$
    declare k int;
    begin
      for k in 1..coalesce(array_length(bs, 1), 0) loop
        begin
          t := convert_from(bs[k], enc); st := null; m := null;
        exception when others then t := null; st := sqlstate; m := sqlerrm;
        end;
        return next;
      end loop;
    end $$""",
    """create function pg_temp.mb_conv(src text, dst text, bs bytea[])
       returns table(b bytea, st text, m text) language plpgsql as $$
    declare k int;
    begin
      for k in 1..coalesce(array_length(bs, 1), 0) loop
        begin
          b := convert(bs[k], src, dst); st := null; m := null;
        exception when others then b := null; st := sqlstate; m := sqlerrm;
        end;
        return next;
      end loop;
    end $$""",
    """create function pg_temp.mb_enc(enc text, lo int, hi int)
       returns table(cp int, b bytea) language plpgsql as $$
    declare k int;
    begin
      for k in lo..hi loop
        continue when k between 55296 and 57343;
        begin
          b := convert_to(chr(k), enc); cp := k; return next;
        exception when others then null;
        end;
      end loop;
    end $$""",
]


def batched(seq: list, n: int = 50000):
    for i in range(0, len(seq), n):
        yield seq[i : i + n]


def code_of(b: bytes) -> int:
    return int.from_bytes(b, "big")


def varint(n: int) -> bytes:
    out = bytearray()
    while True:
        byte = n & 0x7F
        n >>= 7
        if n:
            out.append(byte | 0x80)
        else:
            out.append(byte)
            return bytes(out)


def zigzag(n: int) -> int:
    return (n << 1) if n >= 0 else ((-n) << 1) - 1


def measure(conn: psycopg.Connection, enc: str, dump: Path | None):
    """(decode {code: cp}, pairs {code: (cp1, cp2)}, encode {cp: code},
    the codes the verifier refuses)."""
    decode: dict[int, int] = {}
    pairs: dict[int, tuple[int, int]] = {}
    invalid: set[int] = set()
    lines = []
    for chunk in batched(candidates(enc)):
        rows = conn.execute("select * from pg_temp.mb_dec(%s, %s)", [enc, chunk]).fetchall()
        for b, (text, state, msg) in zip(chunk, rows, strict=True):
            if dump is not None:
                lines.append(
                    f"{b.hex()}\t"
                    + (
                        " ".join(f"{ord(c):x}" for c in text)
                        if text is not None
                        else f"!{state} {msg}"
                    )
                )
            if text is None:
                if state == "22021":
                    invalid.add(code_of(b))
                continue
            cps = [ord(c) for c in text]
            if len(cps) == 1:
                decode[code_of(b)] = cps[0]
            elif len(cps) == 2:
                pairs[code_of(b)] = (cps[0], cps[1])
            else:
                raise AssertionError(f"{enc} {b.hex()} -> {text!r}")
    encode: dict[int, int] = {}
    for lo in range(0x80, 0x110000, 0x10000):
        hi = min(lo + 0xFFFF, 0x10FFFF)
        for cp, b in conn.execute("select * from pg_temp.mb_enc(%s, %s, %s)", [enc, lo, hi]):
            encode[cp] = code_of(bytes(b))
    for code, (a, b) in pairs.items():
        got = conn.execute("select convert_to(chr(%s) || chr(%s), %s)", [a, b, enc]).fetchone()[0]
        assert code_of(bytes(got)) == code, (enc, hex(code))
    if dump is not None:
        (dump / f"{enc}.dec").write_text("\n".join(lines) + "\n")
        (dump / f"{enc}.enc").write_text(
            "".join(f"{cp:x}\t{code:x}\n" for cp, code in sorted(encode.items()))
        )
    return decode, pairs, encode, invalid


def write_encoding(enc: str, decode: dict, pairs: dict, encode: dict) -> int:
    measured_dec, measured_enc = dict(decode), dict(encode)
    if enc == "GB18030":
        # The rule's codes, where the table does not say otherwise: PostgreSQL
        # looks in its table first and uses the rule for what is not there.
        for code in [c for c in decode if c > 0xFFFF and gb18030_rule(c) == decode[c]]:
            if encode.get(decode[code]) == code:
                del encode[decode.pop(code)]
    default: dict[int, int] = {}
    for code in sorted(decode):
        default.setdefault(decode[code], code)
    exceptions = {cp: code for cp, code in encode.items() if default.get(cp) != code}
    exceptions.update({cp: 0 for cp in default if cp not in encode})
    check(enc, measured_dec, pairs, measured_enc, decode, default, exceptions)
    out = bytearray(varint(len(decode)))
    prev_code = prev_cp = 0
    for code in sorted(decode):
        out += varint(code - prev_code) + varint(zigzag(decode[code] - prev_cp))
        prev_code, prev_cp = code, decode[code]
    out += varint(len(pairs))
    for code in sorted(pairs):
        out += varint(code) + varint(pairs[code][0]) + varint(pairs[code][1])
    out += varint(len(exceptions))
    prev_cp = 0
    for cp in sorted(exceptions):
        out += varint(cp - prev_cp) + varint(exceptions[cp])
        prev_cp = cp
    data = zlib.compress(bytes(out), 9)
    (OUT / f"{enc.lower()}.bin").write_bytes(data)
    return len(data)


def gb18030_rule_encode(cp: int) -> int | None:
    """`conv_utf8_to_18030`: the inverse of `gb18030_rule`."""
    for first, lo, hi in GB18030_RANGES:
        if first <= cp <= first + gb_linear(hi) - gb_linear(lo):
            lin = gb_linear(lo) - gb_linear(0x81308130) + cp - first
            return (
                ((0x81 + lin // 12600) << 24)
                | ((0x30 + lin // 1260 % 10) << 16)
                | ((0x81 + lin // 10 % 126) << 8)
                | (0x30 + lin % 10)
            )
    return None


def check(enc, measured_dec, pairs, measured_enc, decode, default, exceptions) -> None:
    """Read the tables back the way `mbcs.rs` does and compare every
    measured answer, so a file that loses something cannot be written."""
    gb = enc == "GB18030"
    for b in candidates(enc):
        code = code_of(b)
        if code in pairs:
            continue
        got = decode.get(code)
        if got is None and gb and len(b) == 4:
            got = gb18030_rule(code)
        assert got == measured_dec.get(code), (enc, b.hex(), got)
    for cp in range(0x80, 0x110000):
        if 0xD800 <= cp <= 0xDFFF:
            continue
        got = exceptions.get(cp, default.get(cp))
        if not got:
            got = gb18030_rule_encode(cp) if gb else None
        assert got == measured_enc.get(cp), (enc, hex(cp), got)


def composed(src_dec: dict, src_pairs: dict, dst_enc: dict, dst_pairs_inv: dict, code: int):
    """What `convert` would give through Unicode: target code, or None."""
    if code in src_pairs:
        return dst_pairs_inv.get(src_pairs[code])
    cp = src_dec.get(code)
    return None if cp is None else dst_enc.get(cp)


def main() -> None:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--only", nargs="*", default=None)
    ap.add_argument("--dump", type=Path, default=None)
    args = ap.parse_args()
    dsn = os.environ.get(
        "SECANTUS_PG_ORACLE_DSN_15", "host=127.0.0.1 port=5415 user=postgres dbname=postgres"
    )
    conn = psycopg.connect(dsn, autocommit=True)
    version = conn.execute("show server_version_num").fetchone()[0]
    assert version.startswith("15"), f"the tables are PostgreSQL 15's, not {version}"
    for stmt in SETUP:
        conn.execute(stmt)
    OUT.mkdir(exist_ok=True)
    if args.dump is not None:
        args.dump.mkdir(parents=True, exist_ok=True)
    wanted = args.only or ENCODINGS
    measured = {}
    for enc in ENCODINGS:
        if enc not in wanted and not any(enc in p and any(w in p for w in wanted) for p in DIRECT):
            continue
        decode, pairs, encode, invalid = measure(conn, enc, args.dump)
        measured[enc] = (dict(decode), dict(pairs), dict(encode), invalid)
        if enc in wanted:
            size = write_encoding(enc, decode, pairs, encode)
            print(
                f"{enc}: {len(decode)} decode, {len(pairs)} pairs, "
                f"{len(encode)} encode, {size} bytes"
            )
    for src, dst in DIRECT:
        if src not in wanted and dst not in wanted:
            continue
        s_dec, s_pairs, _, s_invalid = measured[src]
        _, d_pairs, d_enc, _ = measured[dst]
        d_pairs_inv = {v: k for k, v in d_pairs.items()}
        if dst == "GB18030":
            raise AssertionError("no direct pair targets GB18030")
        diffs: dict[int, int] = {}
        lines = []
        for chunk in batched(candidates(src)):
            rows = conn.execute(
                "select * from pg_temp.mb_conv(%s, %s, %s)", [src, dst, chunk]
            ).fetchall()
            for b, (got, state, msg) in zip(chunk, rows, strict=True):
                code = code_of(b)
                if args.dump is not None:
                    lines.append(
                        f"{b.hex()}\t"
                        + (bytes(got).hex() if got is not None else f"!{state} {msg}")
                    )
                valid = code not in s_invalid
                want = composed(s_dec, s_pairs, d_enc, d_pairs_inv, code)
                have = code_of(bytes(got)) if got is not None else None
                if have != want and (valid or have is not None):
                    diffs[code] = have or 0
                if valid and got is None:
                    shown = " ".join(f"0x{x:02x}" for x in b)
                    expect = (
                        f'invalid byte sequence for encoding "{src}": {shown}'
                        if DIRECT[(src, dst)] == "22021"
                        else f'character with byte sequence {shown} in encoding "{src}" '
                        f'has no equivalent in encoding "{dst}"'
                    )
                    assert (state, msg) == (DIRECT[(src, dst)], expect), (src, dst, b.hex(), msg)
        out = bytearray(varint(len(diffs)))
        prev = 0
        for code in sorted(diffs):
            out += varint(code - prev) + varint(diffs[code])
            prev = code
        data = zlib.compress(bytes(out), 9)
        (OUT / f"{src.lower()}-{dst.lower()}.bin").write_bytes(data)
        if args.dump is not None:
            (args.dump / f"{src}-{dst}.conv").write_text("\n".join(lines) + "\n")
        print(f"{src} -> {dst}: {len(diffs)} differ from the composition, {len(data)} bytes")


if __name__ == "__main__":
    main()
