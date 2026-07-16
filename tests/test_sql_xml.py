"""xml type (#119): well-formedness validation, the xmlelement / xmlforest
constructors, xpath extraction, xml_is_well_formed, and column round-trips.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql, xmltype
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


# --------------------------------------------------------------------------- #
# Pure xmltype.py
# --------------------------------------------------------------------------- #


def test_is_well_formed():
    assert xmltype.is_well_formed("<a/>") is True
    assert xmltype.is_well_formed("<a>x</a>") is True
    assert xmltype.is_well_formed("<a>") is False
    assert xmltype.is_well_formed("not xml") is False


def test_parse_validates():
    assert xmltype.parse("<a>1</a>") == "<a>1</a>"
    with pytest.raises(xmltype.XmlError):
        xmltype.parse("<a>")


def test_element():
    assert xmltype.element("foo", [], ["bar"]) == "<foo>bar</foo>"
    assert xmltype.element("foo", [], []) == "<foo/>"
    assert xmltype.element("item", [("id", "7")], ["hi"]) == '<item id="7">hi</item>'


def test_element_escapes():
    assert xmltype.element("a", [], ["<b>&"]) == "<a>&lt;b&gt;&amp;</a>"
    assert xmltype.element("a", [("k", 'q"v')], []) == '<a k="q&quot;v"/>'


def test_forest():
    assert xmltype.forest([("a", "x"), ("b", "y")]) == "<a>x</a><b>y</b>"
    assert xmltype.forest([("a", "x"), ("b", None)]) == "<a>x</a>"  # NULL skipped


def test_xpath_text():
    xml = "<root><item>one</item><item>two</item></root>"
    assert xmltype.xpath("/root/item/text()", xml) == ["one", "two"]


def test_xpath_element():
    assert xmltype.xpath("/root/item", "<root><item>one</item></root>") == ["<item>one</item>"]


def test_xpath_attr():
    assert xmltype.xpath("/root/@id", '<root id="9"/>') == ["9"]


def test_xpath_descendant():
    xml = "<root><a><b>deep</b></a></root>"
    assert xmltype.xpath("//b/text()", xml) == ["deep"]


def test_xpath_no_match():
    assert xmltype.xpath("/root/missing", "<root><item>x</item></root>") == []


def test_xpath_bad_xml():
    with pytest.raises(xmltype.XmlError):
        xmltype.xpath("/a", "<a>")


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


def test_cast_typed(storage, session):
    assert col(storage, session, "SELECT '<a>1</a>'::xml").type_tag == "xml"


def test_cast_value(storage, session):
    assert val(storage, session, "SELECT '<a>1</a>'::xml") == "<a>1</a>"


def test_xmlelement_typed(storage, session):
    assert col(storage, session, "SELECT xmlelement(name foo, 'bar')").type_tag == "xml"


def test_xmlelement_value(storage, session):
    assert val(storage, session, "SELECT xmlelement(name foo, 'bar')") == "<foo>bar</foo>"


def test_xmlelement_attributes(storage, session):
    got = val(storage, session, "SELECT xmlelement(name item, xmlattributes('7' as id), 'hi')")
    assert got == '<item id="7">hi</item>'


def test_xmlforest(storage, session):
    assert val(storage, session, "SELECT xmlforest('x' as a, 'y' as b)") == "<a>x</a><b>y</b>"


def test_xml_is_well_formed(storage, session):
    assert val(storage, session, "SELECT xml_is_well_formed('<a/>')") is True
    assert val(storage, session, "SELECT xml_is_well_formed('<a>')") is False
    assert col(storage, session, "SELECT xml_is_well_formed('<a/>')").type_tag == "bool"


def test_xpath_typed_array(storage, session):
    assert col(storage, session, "SELECT xpath('/a', '<a/>')").type_tag == "text[]"


def test_xpath_value(storage, session):
    xml = "'<root><item>one</item><item>two</item></root>'"
    assert val(storage, session, f"SELECT xpath('/root/item/text()', {xml})") == ["one", "two"]


def test_xmlconcat(storage, session):
    assert val(storage, session, "SELECT xmlconcat('<a/>', '<b/>')") == "<a/><b/>"


def test_bad_cast_rejected(storage, session):
    with pytest.raises(xmltype.XmlError):
        run(storage, session, "SELECT '<a>'::xml")


@pytest.fixture
def docs(storage, session):
    run(storage, session, "CREATE TABLE docs (id int PRIMARY KEY, body xml)")
    run(storage, session, "INSERT INTO docs VALUES (1, '<doc><title>Hi</title></doc>')")
    return storage


def test_column_roundtrip(docs, session):
    assert (
        val(docs, session, "SELECT body FROM docs WHERE id = 1") == "<doc><title>Hi</title></doc>"
    )


def test_column_typed(docs, session):
    assert col(docs, session, "SELECT body FROM docs WHERE id = 1").type_tag == "xml"


def test_xpath_over_column(docs, session):
    assert val(docs, session, "SELECT xpath('/doc/title/text()', body) FROM docs WHERE id = 1") == [
        "Hi"
    ]
