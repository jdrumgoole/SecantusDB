"""Network address types (#108): inet / cidr / macaddr, the << / >> / &&
containment/overlap operators, and host / masklen / network / netmask /
broadcast / abbrev / family functions.
"""

from __future__ import annotations

import pytest

from secantus.sql import net, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


# --------------------------------------------------------------------------- #
# Pure net.py
# --------------------------------------------------------------------------- #


def test_normalize_inet_keeps_host_bits():
    assert net.normalize_inet("192.168.1.5") == "192.168.1.5/32"
    assert net.normalize_inet("192.168.1.5/24") == "192.168.1.5/24"
    assert net.normalize_inet("::1") == "::1/128"


def test_normalize_cidr_strict():
    assert net.normalize_cidr("192.168.1.0/24") == "192.168.1.0/24"
    with pytest.raises(net.NetError):
        net.normalize_cidr("192.168.1.5/24")  # host bits set


def test_normalize_macaddr_canonical():
    assert net.normalize_macaddr("08-00-2b-01-02-03") == "08:00:2b:01:02:03"
    assert net.normalize_macaddr("08002b:010203") == "08:00:2b:01:02:03"
    with pytest.raises(net.NetError):
        net.normalize_macaddr("08:00:2b")


def test_render_inet_drops_full_mask():
    assert net.render_inet("192.168.1.5/32") == "192.168.1.5"
    assert net.render_inet("192.168.1.5/24") == "192.168.1.5/24"
    assert net.render_inet("::1/128") == "::1"


def test_contains_and_overlaps():
    assert net.contains("10.0.0.0/8", "10.1.2.3/32") is True
    assert net.contains("10.1.2.3/32", "10.0.0.0/8") is False
    assert net.overlaps("10.0.0.0/8", "10.1.0.0/16") is True
    assert net.overlaps("10.0.0.0/8", "11.0.0.0/8") is False
    # Different families never contain / overlap.
    assert net.contains("10.0.0.0/8", "::1/128") is False
    assert net.overlaps("10.0.0.0/8", "::1/128") is False


def test_net_accessors():
    assert net.host("192.168.1.5/24") == "192.168.1.5"
    assert net.masklen("192.168.1.0/24") == 24
    assert net.network("192.168.1.5/24") == "192.168.1.0/24"
    assert net.netmask("192.168.1.0/24") == "255.255.255.0"
    assert net.broadcast("192.168.1.0/24") == "192.168.1.255/24"
    assert net.family("192.168.1.0/24") == 4
    assert net.family("::1/128") == 6


def test_abbrev():
    assert net.abbrev("192.168.1.0/24", is_cidr=True) == "192.168.1.0/24"
    assert net.abbrev("10.0.0.0/8", is_cidr=True) == "10.0.0.0/8"


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


@pytest.fixture
def hosts(storage, session):
    run(storage, session, "CREATE TABLE hosts (id int PRIMARY KEY, addr inet, mac macaddr)")
    run(storage, session, "INSERT INTO hosts VALUES (1, '10.1.2.3/32', '08:00:2b:01:02:03')")
    run(storage, session, "INSERT INTO hosts VALUES (2, '192.168.5.6', '08-00-2b-aa-bb-cc')")
    run(storage, session, "INSERT INTO hosts VALUES (3, '172.16.0.1/16', 'aabb.ccdd.eeff')")
    return storage


def test_inet_cast_typed(storage, session):
    assert col(storage, session, "SELECT '192.168.1.5'::inet").type_tag == "inet"


def test_cidr_cast_typed(storage, session):
    assert col(storage, session, "SELECT '192.168.1.0/24'::cidr").type_tag == "cidr"


def test_macaddr_cast_typed(storage, session):
    assert col(storage, session, "SELECT '08-00-2b-01-02-03'::macaddr").type_tag == "macaddr"


def test_inet_cast_value_normalised(storage, session):
    assert val(storage, session, "SELECT '192.168.1.5'::inet") == "192.168.1.5/32"
    assert val(storage, session, "SELECT '192.168.1.5/24'::inet") == "192.168.1.5/24"


def test_cidr_cast_strict(storage, session):
    assert val(storage, session, "SELECT '10.0.0.0/8'::cidr") == "10.0.0.0/8"


def test_macaddr_cast_canonical(storage, session):
    assert val(storage, session, "SELECT '08-00-2b-01-02-03'::macaddr") == "08:00:2b:01:02:03"


def test_column_roundtrip(hosts, session):
    assert val(hosts, session, "SELECT addr FROM hosts WHERE id = 1") == "10.1.2.3/32"
    assert val(hosts, session, "SELECT mac FROM hosts WHERE id = 1") == "08:00:2b:01:02:03"


def test_contains_op_typed_bool(hosts, session):
    c = col(hosts, session, "SELECT addr << '10.0.0.0/8'::cidr FROM hosts WHERE id = 1")
    assert c.type_tag == "bool"


def test_contains_op_value(hosts, session):
    assert val(hosts, session, "SELECT addr << '10.0.0.0/8'::cidr FROM hosts WHERE id = 1") is True
    assert val(hosts, session, "SELECT '10.0.0.0/8'::cidr >> addr FROM hosts WHERE id = 1") is True
    assert val(hosts, session, "SELECT addr << '10.0.0.0/8'::cidr FROM hosts WHERE id = 2") is False


def test_overlaps_op_value(hosts, session):
    assert val(hosts, session, "SELECT addr && '10.0.0.0/8'::cidr FROM hosts WHERE id = 1") is True
    assert val(hosts, session, "SELECT addr && '11.0.0.0/8'::cidr FROM hosts WHERE id = 1") is False


def test_where_contains(hosts, session):
    ids = [
        r[0]
        for r in run(
            hosts, session, "SELECT id FROM hosts WHERE addr << '10.0.0.0/8'::cidr ORDER BY id"
        ).rows
    ]
    assert ids == [1]


def test_where_overlaps(hosts, session):
    ids = [
        r[0]
        for r in run(
            hosts,
            session,
            "SELECT id FROM hosts WHERE addr && '172.16.0.0/12'::cidr ORDER BY id",
        ).rows
    ]
    assert ids == [3]


def test_host_function(hosts, session):
    assert val(hosts, session, "SELECT host(addr) FROM hosts WHERE id = 3") == "172.16.0.1"


def test_host_typed_text(hosts, session):
    assert col(hosts, session, "SELECT host(addr) FROM hosts WHERE id = 1").type_tag == "text"


def test_masklen_function(hosts, session):
    assert val(hosts, session, "SELECT masklen(addr) FROM hosts WHERE id = 3") == 16


def test_masklen_typed_int(hosts, session):
    assert col(hosts, session, "SELECT masklen(addr) FROM hosts WHERE id = 1").type_tag == "int4"


def test_network_function(hosts, session):
    assert val(hosts, session, "SELECT network(addr) FROM hosts WHERE id = 3") == "172.16.0.0/16"


def test_network_typed_cidr(hosts, session):
    assert col(hosts, session, "SELECT network(addr) FROM hosts WHERE id = 1").type_tag == "cidr"


def test_netmask_function(hosts, session):
    assert val(hosts, session, "SELECT netmask(addr) FROM hosts WHERE id = 3") == "255.255.0.0"


def test_broadcast_function(hosts, session):
    assert (
        val(hosts, session, "SELECT broadcast(addr) FROM hosts WHERE id = 3") == "172.16.255.255/16"
    )


def test_family_function(hosts, session):
    assert val(hosts, session, "SELECT family(addr) FROM hosts WHERE id = 1") == 4


def test_family_typed_int(hosts, session):
    assert col(hosts, session, "SELECT family(addr) FROM hosts WHERE id = 1").type_tag == "int4"
