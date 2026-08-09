"""Geometric types (#115): point / box / circle / polygon / lseg, the <-> / @> /
<@ / && operators, canonical rendering, and column round-trips.
"""

from __future__ import annotations

import struct

import pytest

from secantus.sql import pgextended, pggeo, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


# --------------------------------------------------------------------------- #
# Pure pggeo.py
# --------------------------------------------------------------------------- #


def test_canonical():
    assert pggeo.canonical("(1, 2)", "point") == "(1,2)"
    assert pggeo.canonical("((0,0),(2,2))", "box") == "(2,2),(0,0)"
    assert pggeo.canonical("<(0,0),5>", "circle") == "<(0,0),5>"
    assert pggeo.canonical("((0,0),(1,0),(1,1))", "polygon") == "((0,0),(1,0),(1,1))"
    assert pggeo.canonical("[(0,0),(3,4)]", "lseg") == "[(0,0),(3,4)]"


def test_canonical_rejects_garbage():
    with pytest.raises(pggeo.GeoError):
        pggeo.canonical("not a point", "point")


def test_distance():
    assert pggeo.distance("(0,0)", "(3,4)") == 5
    assert pggeo.distance("(10,0)", "<(0,0),5>") == 5  # point to circle edge
    assert pggeo.distance("[(0,0),(0,10)]", "(5,5)") == 5  # point to segment


def test_contains():
    assert pggeo.contains("(2,2),(0,0)", "(1,1)") is True  # box contains point
    assert pggeo.contains("(2,2),(0,0)", "(3,3)") is False
    assert pggeo.contains("<(0,0),5>", "(3,3)") is True  # circle contains point (dist ~4.24)
    assert pggeo.contains("<(0,0),5>", "(4,4)") is False  # dist ~5.66


def test_overlaps():
    assert pggeo.overlaps("((0,0),(2,0),(2,2),(0,2))", "((1,1),(3,1),(3,3),(1,3))") is True
    assert pggeo.overlaps("((0,0),(1,0),(1,1))", "((5,5),(6,5),(6,6))") is False


def test_is_geo_text():
    assert pggeo.is_geo_text("(1,2)") is True
    assert pggeo.is_geo_text("<(0,0),5>") is True
    assert pggeo.is_geo_text("hello") is False
    assert pggeo.is_geo_text("19.99") is False


# --------------------------------------------------------------------------- #
# SQL surface
# --------------------------------------------------------------------------- #


@pytest.fixture
def session():
    return Session(database=DB, user="secantus")


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def val(storage, session, sql):
    return run(storage, session, sql).rows[0][0]


def col(storage, session, sql):
    return run(storage, session, sql).columns[0]


def test_point_cast_typed(storage, session):
    assert col(storage, session, "SELECT '(1,2)'::point").type_tag == "point"


def test_geo_casts_canonicalise(storage, session):
    assert val(storage, session, "SELECT '(1, 2)'::point") == "(1,2)"
    assert val(storage, session, "SELECT '((0,0),(2,2))'::box") == "(2,2),(0,0)"
    assert val(storage, session, "SELECT '<(0,0),5>'::circle") == "<(0,0),5>"


def test_box_typed(storage, session):
    assert col(storage, session, "SELECT '((0,0),(2,2))'::box").type_tag == "box"


def test_distance_typed_float(storage, session):
    assert col(storage, session, "SELECT point '(0,0)' <-> point '(3,4)'").type_tag == "float8"


def test_distance_value(storage, session):
    assert val(storage, session, "SELECT point '(0,0)' <-> point '(3,4)'") == 5


def test_contains_typed_bool(storage, session):
    c = col(storage, session, "SELECT '((0,0),(2,2))'::box @> point '(1,1)'")
    assert c.type_tag == "bool"


def test_box_contains_point(storage, session):
    assert val(storage, session, "SELECT '((0,0),(2,2))'::box @> point '(1,1)'") is True
    assert val(storage, session, "SELECT '((0,0),(2,2))'::box @> point '(3,3)'") is False


def test_circle_contains_point(storage, session):
    assert val(storage, session, "SELECT '<(0,0),5>'::circle @> point '(3,3)'") is True


def test_overlaps_op(storage, session):
    poly1 = "'((0,0),(2,0),(2,2),(0,2))'::polygon"
    poly2 = "'((1,1),(3,1),(3,3),(1,3))'::polygon"
    poly3 = "'((9,9),(10,9),(10,10),(9,10))'::polygon"
    assert val(storage, session, f"SELECT {poly1} && {poly2}") is True
    assert val(storage, session, f"SELECT {poly1} && {poly3}") is False


@pytest.fixture
def shapes(storage, session):
    run(storage, session, "CREATE TABLE shapes (id int PRIMARY KEY, loc point, area polygon)")
    run(storage, session, "INSERT INTO shapes VALUES (1, '(1,1)', '((0,0),(4,0),(4,4),(0,4))')")
    run(storage, session, "INSERT INTO shapes VALUES (2, '(9,9)', '((5,5),(6,5),(6,6),(5,6))')")
    return storage


def test_geo_column_roundtrip(shapes, session):
    assert val(shapes, session, "SELECT loc FROM shapes WHERE id = 1") == "(1,1)"


def test_geo_column_typed(shapes, session):
    assert col(shapes, session, "SELECT loc FROM shapes WHERE id = 1").type_tag == "point"


def test_distance_in_select_orderby(shapes, session):
    rows = run(shapes, session, "SELECT id FROM shapes ORDER BY loc <-> point '(0,0)'").rows
    assert [r[0] for r in rows] == [1, 2]


def test_where_polygon_contains_point(shapes, session):
    ids = [
        r[0]
        for r in run(
            shapes, session, "SELECT id FROM shapes WHERE area @> point '(2,2)' ORDER BY id"
        ).rows
    ]
    assert ids == [1]


# --------------------------------------------------------------------------- #
# line ``{A,B,C}`` and binary-format parameters
# --------------------------------------------------------------------------- #


def test_canonical_line():
    """``line``'s canonical text is three coefficients, not coordinate pairs —
    the branch handling it used to sit *after* the pair parse, so every line
    literal raised ``no coordinate pairs`` instead of being accepted."""
    assert pggeo.canonical("{0.0,0.0,0.0}", "line") == "{0,0,0}"
    assert pggeo.canonical("{1,2,3}", "line") == "{1,2,3}"
    # The two-point spelling is accepted on input and converted, like Postgres.
    assert pggeo.canonical("[(0,0),(1,1)]", "line") == "{1,-1,0}"
    assert pggeo.canonical("[(0,0),(0,5)]", "line") == "{-1,0,0}"  # vertical
    assert pggeo.canonical("[(0,0),(5,0)]", "line") == "{0,-1,0}"  # horizontal


def test_canonical_line_rejects_garbage():
    with pytest.raises(pggeo.GeoError):
        pggeo.canonical("{1,2}", "line")
    with pytest.raises(pggeo.GeoError):
        pggeo.canonical("[(1,1),(1,1)]", "line")  # not two distinct points


def test_path_keeps_open_closed_spelling():
    assert pggeo.canonical("[(0,0),(1,1)]", "path") == "[(0,0),(1,1)]"
    assert pggeo.canonical("((0,0),(1,1))", "path") == "((0,0),(1,1))"


def test_line_has_no_shapely_form():
    """An infinite line has no Shapely counterpart; the operators must say so
    rather than fail deep inside the pair parser."""
    with pytest.raises(pggeo.GeoError, match="not supported on line"):
        pggeo.to_shapely("{1,-1,0}")


@pytest.mark.parametrize(
    ("oid", "raw", "expected"),
    [
        (600, struct.pack("!2d", 1.5, -2.0), "(1.5,-2)"),  # point
        (601, struct.pack("!4d", 1, 2, 3, 4), "[(1,2),(3,4)]"),  # lseg
        (603, struct.pack("!4d", 3, 4, 1, 2), "(3,4),(1,2)"),  # box
        (628, struct.pack("!3d", 0.0, 0.0, 0.0), "{0,0,0}"),  # line
        (718, struct.pack("!3d", 1, 2, 3), "<(1,2),3>"),  # circle
        (
            604,
            struct.pack("!i", 3) + struct.pack("!6d", 0, 0, 1, 0, 1, 1),
            "((0,0),(1,0),(1,1))",
        ),  # polygon
        (
            602,
            b"\x01" + struct.pack("!i", 2) + struct.pack("!4d", 0, 0, 1, 1),
            "((0,0),(1,1))",
        ),  # path, closed
        (
            602,
            b"\x00" + struct.pack("!i", 2) + struct.pack("!4d", 0, 0, 1, 1),
            "[(0,0),(1,1)]",
        ),  # path, open
    ],
)
def test_geometric_binary_parameters(oid, raw, expected):
    """A geometric parameter sent in binary (pgjdbc's default for these types)
    had no decoder, so the raw bytes reached the *text* parser and the insert
    died with ``no coordinate pairs``."""
    assert str(pgextended._BINARY[oid](raw)) == expected
