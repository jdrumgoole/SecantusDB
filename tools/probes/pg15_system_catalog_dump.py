"""Dump PostgreSQL 15.19's system views and static catalog rows for the Rust
PG server (RP-070).

Run against a FRESH database on a PostgreSQL 15.19 cluster (nothing created
in it), so every row is one ``initdb`` made:

    python tools/probes/pg15_system_catalog_dump.py "host=... port=... dbname=fresh"

Writes three files the server compiles in:

* ``crates/secantus-pgplan/src/pg15_system_views.tsv`` -- ``schema.view``,
  its oid, then ``pg_get_viewdef(view, true)`` with ``\\``, tab and newline escaped.
  The planner expands a system view from it as the rewriter expands any
  view.
* ``crates/secantus-pgplan/src/pg15_information_schema_functions.tsv`` --
  ``information_schema``'s scalar SQL helpers, which the planner inlines.
* ``crates/secantus-pg/src/pg15_catalog_rows.txt`` -- for each table in
  ``STATIC_TABLES``, a ``\\relation schema.table`` line and then the table's
  rows in COPY text format, in ``ctid`` order, a ``reg*`` column as its oid.
* ``crates/secantus-pg/src/pg15_catalog_statistics.txt`` -- the
  ``pg_statistic`` rows ``initdb``'s ANALYZE left for the built-in catalogs
  (RP-311), in COPY text format: every column up to ``stanumbers5``, then
  for each slot its values' element type oid and the values as
  ``array_to_json`` (an ``anyarray`` has no other way out).
* ``crates/secantus-pg/src/pg15_catalog_indexes.txt`` -- every built-in
  catalog index's B-tree pages as ``initdb`` left them (RP-291), read from
  the cluster's data files, so the cluster must be LOCAL and the user a
  superuser: a ``\\index oid root level`` line, then one line per page,
  ``block flags next level items``, where a leaf's items are each index
  tuple's heap TID count (``1x40`` is forty plain tuples; a posting list
  counts its TIDs) and an internal page's are its downlinks' blocks. A
  non-rightmost page's high key is not an item.
* ``crates/secantus-pg/src/pg15_catalog_heaps.txt`` -- every built-in
  catalog's heap as ``initdb`` left it (RP-291): a ``\\heap oid name blocks``
  line, then one line per block holding live tuples, ``block offsets``, the
  offsets as ranges (``1-57,60``) in ``ctid`` order. A row of the rows file
  above is the table's n-th live tuple here. Each heap's pages follow, as
  ``heap_pages`` writes them: free space, line pointers and the free space
  map, read from the data files (a LOCAL cluster, a superuser).
"""

from __future__ import annotations

import pathlib
import struct
import sys

import psycopg

ROOT = pathlib.Path(__file__).resolve().parents[2]

# The system views the server does not compute itself: each is served from
# PostgreSQL's own definition, over the server's catalogs.
VIEWS = [
    "information_schema._pg_foreign_data_wrappers",
    "information_schema._pg_foreign_servers",
    "information_schema._pg_foreign_table_columns",
    "information_schema._pg_foreign_tables",
    "information_schema._pg_user_mappings",
    "information_schema.administrable_role_authorizations",
    "information_schema.applicable_roles",
    "information_schema.attributes",
    "information_schema.character_sets",
    "information_schema.check_constraint_routine_usage",
    "information_schema.check_constraints",
    "information_schema.collation_character_set_applicability",
    "information_schema.collations",
    "information_schema.column_column_usage",
    "information_schema.column_domain_usage",
    "information_schema.column_options",
    "information_schema.column_privileges",
    "information_schema.column_udt_usage",
    "information_schema.constraint_column_usage",
    "information_schema.constraint_table_usage",
    "information_schema.data_type_privileges",
    "information_schema.domain_constraints",
    "information_schema.domain_udt_usage",
    "information_schema.domains",
    "information_schema.element_types",
    "information_schema.enabled_roles",
    "information_schema.foreign_data_wrapper_options",
    "information_schema.foreign_server_options",
    "information_schema.foreign_table_options",
    "information_schema.information_schema_catalog_name",
    "information_schema.parameters",
    "information_schema.referential_constraints",
    "information_schema.role_column_grants",
    "information_schema.role_routine_grants",
    "information_schema.role_udt_grants",
    "information_schema.role_usage_grants",
    "information_schema.routine_column_usage",
    "information_schema.routine_privileges",
    "information_schema.routine_routine_usage",
    "information_schema.routine_sequence_usage",
    "information_schema.routine_table_usage",
    "information_schema.routines",
    "information_schema.schemata",
    "information_schema.transforms",
    "information_schema.triggered_update_columns",
    "information_schema.triggers",
    "information_schema.udt_privileges",
    "information_schema.usage_privileges",
    "information_schema.user_defined_types",
    "information_schema.user_mapping_options",
    "information_schema.user_mappings",
    "information_schema.view_column_usage",
    "information_schema.view_routine_usage",
    "information_schema.view_table_usage",
    "pg_catalog.pg_group",
    "pg_catalog.pg_seclabels",
    "pg_catalog.pg_shadow",
    "pg_catalog.pg_stats",
    # The cumulative statistics views, over the server's counters (RP-070).
    "pg_catalog.pg_stat_all_indexes",
    "pg_catalog.pg_stat_all_tables",
    "pg_catalog.pg_stat_database",
    "pg_catalog.pg_stat_sys_indexes",
    "pg_catalog.pg_stat_sys_tables",
    "pg_catalog.pg_stat_user_functions",
    "pg_catalog.pg_stat_user_indexes",
    "pg_catalog.pg_stat_user_tables",
    "pg_catalog.pg_stat_xact_all_tables",
    "pg_catalog.pg_stat_xact_sys_tables",
    "pg_catalog.pg_stat_xact_user_functions",
    "pg_catalog.pg_stat_xact_user_tables",
    "pg_catalog.pg_statio_all_indexes",
    "pg_catalog.pg_statio_all_sequences",
    "pg_catalog.pg_statio_all_tables",
    "pg_catalog.pg_statio_sys_indexes",
    "pg_catalog.pg_statio_sys_sequences",
    "pg_catalog.pg_statio_sys_tables",
    "pg_catalog.pg_statio_user_indexes",
    "pg_catalog.pg_statio_user_sequences",
    "pg_catalog.pg_statio_user_tables",
    # The progress views, over the backends' progress slots (RP-100).
    "pg_catalog.pg_stat_progress_analyze",
    "pg_catalog.pg_stat_progress_basebackup",
    "pg_catalog.pg_stat_progress_cluster",
    "pg_catalog.pg_stat_progress_copy",
    "pg_catalog.pg_stat_progress_create_index",
    "pg_catalog.pg_stat_progress_vacuum",
]

# Tables whose rows are PostgreSQL's built-in objects and nothing else on a
# fresh database.
STATIC_TABLES = [
    "information_schema.sql_features",
    "information_schema.sql_implementation_info",
    "information_schema.sql_parts",
    "information_schema.sql_sizing",
    "pg_catalog.pg_aggregate",
    "pg_catalog.pg_amop",
    "pg_catalog.pg_amproc",
    "pg_catalog.pg_conversion",
    "pg_catalog.pg_init_privs",
    "pg_catalog.pg_opclass",
    "pg_catalog.pg_opfamily",
    # Every built-in function, type and catalog constraint (RP-102); the
    # server lists its users' objects after them.
    "pg_catalog.pg_proc",
    "pg_catalog.pg_type",
    "pg_catalog.pg_constraint",
    # Every built-in operator, and the system views' rules (RP-211, RP-203).
    "pg_catalog.pg_operator",
    "pg_catalog.pg_rewrite",
]


def escape(s: str) -> str:
    return s.replace("\\", "\\\\").replace("\t", "\\t").replace("\n", "\\n")


def main(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as conn:
        version = conn.execute("show server_version").fetchone()[0]
        if version.split()[0] != "15.19":
            raise SystemExit(f"need PostgreSQL 15.19, connected to {version}")
        lines = []
        for view in VIEWS:
            oid, definition = conn.execute(
                "select %s::regclass::oid, pg_get_viewdef(%s::regclass, true)", (view, view)
            ).fetchone()
            lines.append(f"{view}\t{oid}\t{escape(definition.strip())}\n")
        (ROOT / "crates/secantus-pgplan/src/pg15_system_views.tsv").write_text("".join(lines))

        # information_schema's scalar SQL helpers, for the planner to inline:
        # name, parameter names, argument count, body as an expression.
        helpers = []
        for name, params, nargs, body in conn.execute(
            "select p.proname, coalesce(array_to_string(p.proargnames, ','), ''),"
            " p.pronargs, pg_get_function_sqlbody(p.oid) from pg_proc p"
            " where p.pronamespace = 'information_schema'::regnamespace"
            " and not p.proretset order by 1"
        ):
            body = body.strip()
            if body.startswith("RETURN "):
                expr = body.removeprefix("RETURN ")
            else:
                select = body.removeprefix("BEGIN ATOMIC").removesuffix("END")
                expr = f"({select.strip().rstrip(';')})"
            helpers.append(f"{name}\t{params}\t{nargs}\t{escape(expr)}\n")
        (ROOT / "crates/secantus-pgplan/src/pg15_information_schema_functions.tsv").write_text(
            "".join(helpers)
        )

        out = []
        for table in STATIC_TABLES:
            out.append(f"\\relation {table}\n")
            # A reg* column as its oid, which the server renders by name; an
            # expression (pg_node_tree) as the SQL text the server keeps for
            # its own objects' expressions.
            columns = conn.execute(
                "select case when atttypid::regtype::text like 'reg%%'"
                " then quote_ident(attname) || '::oid'"
                " when atttypid = 'pg_node_tree'::regtype then"
                " case attname when 'conbin' then 'pg_get_expr(conbin, conrelid)'"
                # A rule's node trees are not kept here (RP-203).
                " when 'ev_qual' then 'NULL' when 'ev_action' then 'NULL'"
                " when 'prosqlbody' then 'pg_get_function_sqlbody(oid)'"
                " else 'pg_get_expr(' || quote_ident(attname) || ', 0)' end"
                " else quote_ident(attname) end"
                " from pg_attribute where attrelid = %s::regclass and attnum > 0"
                " order by attnum",
                (table,),
            ).fetchall()
            select = ", ".join(c for (c,) in columns)
            query = f"copy (select {select} from {table} order by ctid) to stdout"
            with conn.cursor().copy(query) as copy:
                for block in copy:
                    out.append(bytes(block).decode())
        (ROOT / "crates/secantus-pg/src/pg15_catalog_rows.txt").write_text("".join(out))

        statistics(conn)
        class_sizes(conn)
        index_pages(conn)
        heap_tids(conn)


def statistics(conn: psycopg.Connection) -> None:
    slots = range(1, 6)
    cols = ", ".join(
        [
            "s.starelid",
            "s.staattnum",
            "s.stainherit",
            "s.stanullfrac",
            "s.stawidth",
            "s.stadistinct",
        ]
        + [f"s.{c}{n}" for c in ("stakind", "staop", "stacoll", "stanumbers") for n in slots]
        # A most-common-elements slot (kind 4) holds the array's elements.
        + [
            f"case when s.stavalues{n} is null then null when s.stakind{n} = 4"
            f" then t.typelem else a.atttypid end, array_to_json(s.stavalues{n})"
            for n in slots
        ]
    )
    query = (
        f"copy (select {cols} from pg_statistic s join pg_attribute a"
        " on a.attrelid = s.starelid and a.attnum = s.staattnum"
        " join pg_type t on t.oid = a.atttypid where s.starelid < 16384"
        " order by s.ctid) to stdout"
    )
    out = []
    with conn.cursor().copy(query) as copy:
        for block in copy:
            out.append(bytes(block).decode())
    (ROOT / "crates/secantus-pg/src/pg15_catalog_statistics.txt").write_text("".join(out))


def class_sizes(conn: psycopg.Connection) -> None:
    """Each built-in relation's `relpages`, `reltuples` and `relallvisible`
    as `initdb`'s VACUUM left them, which the planner reads (RP-314)."""
    out = [
        f"{oid}\t{pages}\t{tuples}\t{allvisible}\n"
        for oid, pages, tuples, allvisible in conn.execute(
            "select oid, relpages, reltuples::text, relallvisible from pg_class"
            " where oid < 16384 order by oid"
        )
    ]
    (ROOT / "crates/secantus-pg/src/pg15_class_sizes.tsv").write_text("".join(out))


BLCKSZ = 8192
BTP_LEAF, BTP_META, BTP_DELETED = 1, 8, 4
INDEX_ALT_TID_MASK, BT_IS_POSTING, BT_OFFSET_MASK = 0x2000, 0x2000, 0x0FFF


def page_items(
    page: bytes,
) -> tuple[int, int, int, list[int], list[int], list[list[tuple[int, int]]], list[bytes]]:
    """A B-tree page's flags, right link, level, and per data item (the high
    key left out) its heap TID count on a leaf or its downlink's block on an
    internal page, and a leaf item's heap TIDs (``nbtree.h``, ``itup.h``,
    ``bufpage.h``), with each leaf item's bytes and its data offset."""
    lower = struct.unpack_from("<H", page, 12)[0]
    special = struct.unpack_from("<H", page, 16)[0]
    _prev, nxt, level, flags = struct.unpack_from("<IIIH", page, special)
    counts: list[int] = []
    downlinks: list[int] = []
    tids: list[list[tuple[int, int]]] = []
    datas: list[bytes] = []
    first = 1 if nxt != 0 else 0  # P_FIRSTDATAKEY: a high key on a non-rightmost page
    for n, off in enumerate(range(24, lower, 4)):
        lp = struct.unpack_from("<I", page, off)[0]
        lp_off, lp_flags = lp & 0x7FFF, (lp >> 15) & 3
        if n < first or lp_flags == 0:
            continue
        bi_hi, bi_lo, posid, info = struct.unpack_from("<HHHH", page, lp_off)
        block = (bi_hi << 16) | bi_lo
        if flags & BTP_LEAF:
            posting = info & INDEX_ALT_TID_MASK and posid & BT_IS_POSTING
            if posting:
                n = posid & BT_OFFSET_MASK
                at = lp_off + block  # BTreeTupleGetPostingOffset
                item_tids = []
                for k in range(n):
                    hi, lo, off = struct.unpack_from("<HHH", page, at + 6 * k)
                    item_tids.append(((hi << 16) | lo, off))
            else:
                item_tids = [(block, posid)]
            counts.append(len(item_tids))
            tids.append(item_tids)
            size = info & 0x1FFF
            header = 16 if info & 0x8000 else 8  # INDEX_NULL_MASK: a null bitmap
            datas.append(page[lp_off : lp_off + size] + bytes([header]))
        else:
            downlinks.append(block)
    return flags, nxt, level, counts, downlinks, tids, datas


def decode_key(raw: bytes, types: list[tuple[int, int, str, str]]) -> list[str | None]:
    """An index tuple's key values as their types' text output, read from
    its bytes (``index_getattr``): each at its type's alignment, a varlena
    unaligned when it has a 1-byte header. `types` holds each column's
    ``(oid, typlen, typalign, typname)``; `raw` ends with the data offset."""
    data, offset = raw[:-1], raw[-1]
    info = struct.unpack_from("<H", data, 6)[0]
    nulls = data[8] if info & 0x8000 else 0
    out: list[str | None] = []
    at = offset
    align = {"c": 1, "s": 2, "i": 4, "d": 8}
    for n, (_oid, length, alignment, name) in enumerate(types):
        if info & 0x8000 and not nulls & (1 << n):
            out.append(None)
            continue
        if name == "name":
            # `name_ops` stores a `cstring` (`opckeytype`): unaligned, its
            # bytes to the terminating NUL.
            end = data.index(b"\0", at)
            out.append(data[at:end].decode())
            at = end + 1
            continue
        if length == -1:
            if at < len(data) and data[at] & 1 and data[at] != 1:
                size = data[at] >> 1
                out.append(varlena(name, data[at + 1 : at + size]))
                at += size
                continue
            a = align[alignment]
            at = (at + a - 1) // a * a
            size = struct.unpack_from("<I", data, at)[0] >> 2
            out.append(varlena(name, data[at + 4 : at + size]))
            at += size
            continue
        a = align[alignment]
        at = (at + a - 1) // a * a
        chunk = data[at : at + length]
        at += length
        if name == "char":
            out.append(chunk.decode("latin-1"))
        elif name == "bool":
            out.append("t" if chunk[0] else "f")
        elif length == 2:
            out.append(str(struct.unpack("<h", chunk)[0]))
        elif length == 8:
            out.append(str(struct.unpack("<q", chunk)[0]))
        elif name in ("int4",):
            out.append(str(struct.unpack("<i", chunk)[0]))
        else:
            out.append(str(struct.unpack("<I", chunk)[0]))
    return out


def varlena(name: str, body: bytes) -> str:
    """A varlena key's text: an ``oidvector`` / ``int2vector``'s elements
    (after ``ndim``, ``dataoffset``, ``elemtype``, ``dim1``, ``lbound1``),
    else the bytes as text."""
    if name in ("oidvector", "int2vector"):
        dim = struct.unpack_from("<i", body, 12)[0] if len(body) >= 20 else 0
        fmt = "<I" if name == "oidvector" else "<h"
        width = 4 if name == "oidvector" else 2
        return " ".join(str(struct.unpack_from(fmt, body, 20 + width * k)[0]) for k in range(dim))
    return body.decode()


def run_length(counts: list[int]) -> str:
    out: list[str] = []
    i = 0
    while i < len(counts):
        j = i
        while j < len(counts) and counts[j] == counts[i]:
            j += 1
        out.append(f"{counts[i]}x{j - i}" if j - i > 1 else str(counts[i]))
        i = j
    return ",".join(out)


def index_pages(conn: psycopg.Connection) -> None:
    conn.execute("checkpoint")
    datadir = pathlib.Path(conn.execute("show data_directory").fetchone()[0])
    out = []
    for oid, path, types in conn.execute(
        "select c.oid, pg_relation_filepath(c.oid),"
        " (select array_agg(array[t.oid::text, t.typlen::text, t.typalign::text, t.typname::text]"
        " order by k.n) from unnest(i.indkey) with ordinality k(att, n)"
        " join pg_attribute a on a.attrelid = i.indrelid and a.attnum = k.att"
        " join pg_type t on t.oid = a.atttypid where k.n <= i.indnkeyatts)"
        " from pg_class c join pg_index i on i.indexrelid = c.oid"
        " where c.relkind = 'i' and c.oid < 16384"
        " and c.relam = (select oid from pg_am where amname = 'btree') order by 1"
    ).fetchall():
        key_types = [(int(t[0]), int(t[1]), t[2], t[3]) for t in types]
        data = (datadir / path).read_bytes()
        root, level = struct.unpack_from("<II", data, 24 + 8)  # btm_magic, btm_version, then these
        out.append(f"\\index {oid} {root} {level} {' '.join(str(t[0]) for t in key_types)}\n")
        for blk in range(1, len(data) // BLCKSZ):
            flags, nxt, lvl, counts, downlinks, tids, datas = page_items(
                data[blk * BLCKSZ : (blk + 1) * BLCKSZ]
            )
            items = run_length(counts) if flags & BTP_LEAF else ",".join(map(str, downlinks))
            out.append(f"{blk} {flags} {nxt} {lvl} {items}\n")
            # A leaf's tuples: `= tid[,tid...]` then each key value, `\\N`
            # for NULL, as its type's text output.
            for item_tids, raw in zip(tids, datas, strict=True) if flags & BTP_LEAF else []:
                values = decode_key(raw, key_types)
                cells = ["\\N" if v is None else escape(v) for v in values]
                tid_text = ",".join(f"{b}.{o}" for b, o in item_tids)
                out.append("= " + "\t".join([tid_text, *cells]) + "\n")
    (ROOT / "crates/secantus-pg/src/pg15_catalog_indexes.txt").write_text("".join(out))


def ranges(offsets: list[int]) -> str:
    out: list[str] = []
    i = 0
    while i < len(offsets):
        j = i
        while j + 1 < len(offsets) and offsets[j + 1] == offsets[j] + 1:
            j += 1
        out.append(f"{offsets[i]}-{offsets[j]}" if j > i else str(offsets[i]))
        i = j + 1
    return ",".join(out)


def heap_pages(datadir: pathlib.Path, path: str) -> list[str]:
    """Each heap page as the page model keeps it (RP-291): a ``\\page block
    free nlp`` line (``pd_upper - pd_lower``, the line pointer count), then
    each line pointer that is not an ordinary tuple, ``off:u`` unused,
    ``off:r>to`` a redirect, ``off:h<len>`` a heap-only tuple of its
    MAXALIGNed length (``bufpage.h``, ``itemid.h``, ``htup_details.h``);
    then, when the fork exists, ``\\fsm nblocks`` and each free space map
    page as ``\\fsmpage block fp_next_slot node:value,...``, its nonzero
    nodes (``fsm_internals.h``): the fork exactly, stale upper nodes and
    all."""
    data = (datadir / path).read_bytes()
    out = []
    for blk in range(len(data) // BLCKSZ):
        page = data[blk * BLCKSZ : (blk + 1) * BLCKSZ]
        lower, upper = struct.unpack_from("<HH", page, 12)
        nlp = (lower - 24) // 4
        exc = []
        for i in range(nlp):
            word = struct.unpack_from("<I", page, 24 + 4 * i)[0]
            off, flags, length = word & 0x7FFF, (word >> 15) & 3, word >> 17
            if flags == 0:
                exc.append(f"{i + 1}:u")
            elif flags == 2:
                exc.append(f"{i + 1}:r>{off}")
            elif flags == 3:
                raise SystemExit(f"{path} block {blk}: a dead line pointer")
            else:
                infomask2 = struct.unpack_from("<H", page, off + 18)[0]
                if infomask2 & 0x4000:
                    raise SystemExit(f"{path} block {blk}: a HOT-updated live tuple")
                if infomask2 & 0x8000:
                    exc.append(f"{i + 1}:h{(length + 7) & ~7}")
        out.append(f"\\page {blk} {upper - lower} {nlp}{''.join(' ' + e for e in exc)}\n")
    fsm = datadir / (path + "_fsm")
    if fsm.exists():
        raw = fsm.read_bytes()
        out.append(f"\\fsm {len(raw) // BLCKSZ}\n")
        for blk in range(len(raw) // BLCKSZ):
            page = raw[blk * BLCKSZ : (blk + 1) * BLCKSZ]
            next_slot = struct.unpack_from("<i", page, 24)[0]
            nodes = page[28:BLCKSZ]
            cells = ",".join(f"{n}:{v}" for n, v in enumerate(nodes) if v)
            out.append(f"\\fsmpage {blk} {next_slot} {cells}\n")
    return out


def heap_tids(conn: psycopg.Connection) -> None:
    """Each built-in catalog's live tuples' ``ctid``, by block (RP-291)."""
    conn.execute("checkpoint")
    datadir = pathlib.Path(conn.execute("show data_directory").fetchone()[0])
    out = []
    for oid, name, path in conn.execute(
        "select c.oid, c.relname, pg_relation_filepath(c.oid) from pg_class c where c.relkind = 'r'"
        " and c.relnamespace = 'pg_catalog'::regnamespace and c.oid < 16384 order by 1"
    ).fetchall():
        blocks = conn.execute("select pg_relation_size(%s) / 8192", (oid,)).fetchone()[0]
        out.append(f"\\heap {oid} {name} {blocks}\n")
        by_block: dict[int, list[int]] = {}
        for (tid,) in conn.execute(f"select t.ctid::text from pg_catalog.{name} t order by t.ctid"):
            blk, off = tid.strip("()").split(",")
            by_block.setdefault(int(blk), []).append(int(off))
        for blk, offs in by_block.items():
            out.append(f"{blk} {ranges(offs)}\n")
        out.extend(heap_pages(datadir, path))
    (ROOT / "crates/secantus-pg/src/pg15_catalog_heaps.txt").write_text("".join(out))


if __name__ == "__main__":
    main(sys.argv[1])
