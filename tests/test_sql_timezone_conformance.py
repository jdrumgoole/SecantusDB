"""Session-zone conformance for the pgjdbc TimezoneTest / DateTest matrix.

Five fixes pinned here, each measured against the pgjdbc gauge (TimezoneTest
16/16, DateTest 192/192 after them):

* the ``TimeZone`` STARTUP parameter is honoured (pgjdbc sends the JVM zone
  that way; dropping it left every JDBC session on UTC, shifting date reads
  a day for clients west of Greenwich);
* ``TimeZone`` values normalize like Postgres reports them (``gmt-3`` ->
  ``GMT-3``) — pgjdbc's ParameterStatus parser matches ``GMT±`` case
  sensitively and fell back to UTC otherwise;
* POSIX-style GMT offsets accept minutes (``GMT+3:30`` is UTC-03:30);
* ``tstz::text`` / ``tz::text`` casts render Postgres' spellings
  (session-zone offset on timestamptz; ``+01`` not ``+01:00`` on timetz);
* an out-of-Python-range (BC) timestamptz literal without an offset is
  stamped with the session zone's offset, so the stored instant is right.

Everything runs against the real WiredTiger-backed ``Storage``.
"""

from __future__ import annotations

import datetime as dt
import socket
import struct

import pytest

from secantus.sql import pgwire, run_sql, typemap
from secantus.sql.datetimes import tzinfo_for_setting
from secantus.sql.pgserver import SecantusPGServer
from secantus.sql.session import Session, canonical_timezone_setting
from secantus.storage import Storage

DB = "d"


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


@pytest.fixture
def session():
    sess = Session(database=DB)
    typemap.set_render_session(sess)
    try:
        yield sess
    finally:
        typemap.set_render_session(None)


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


# --------------------------------------------------------------------------- #
# TimeZone GUC parsing / normalization
# --------------------------------------------------------------------------- #


def test_posix_gmt_offsets_invert_sign():
    assert tzinfo_for_setting("GMT+3").utcoffset(None) == dt.timedelta(hours=-3)
    assert tzinfo_for_setting("GMT-3").utcoffset(None) == dt.timedelta(hours=3)
    # Half-hour POSIX zones (pgjdbc's halfHourTimezone): GMT+3:30 is UTC-03:30.
    assert tzinfo_for_setting("GMT+3:30").utcoffset(None) == dt.timedelta(hours=-3, minutes=-30)
    assert tzinfo_for_setting("UTC-5:45").utcoffset(None) == dt.timedelta(hours=5, minutes=45)


def test_canonical_timezone_setting():
    assert canonical_timezone_setting("gmt-3") == "GMT-3"
    assert canonical_timezone_setting("utc") == "UTC"
    assert canonical_timezone_setting("gmt") == "GMT"
    assert canonical_timezone_setting("Europe/Paris") == "Europe/Paris"
    assert canonical_timezone_setting("GMT+12") == "GMT+12"


def test_set_timezone_reports_canonical_spelling(storage, session):
    r = q(storage, session, "set timezone = 'gmt-3'")
    assert ("TimeZone", "GMT-3") in r.parameter_status
    assert q(storage, session, "SHOW TimeZone").rows == [("GMT-3",)]


def test_halfhour_zone_literal_is_epoch(storage, session):
    q(storage, session, "SET TimeZone = 'GMT+3:30'")
    v = q(storage, session, "SELECT '1969-12-31 20:30:00'::timestamptz").rows[0][0]
    assert v.timestamp() == 0.0


# --------------------------------------------------------------------------- #
# ::text renders
# --------------------------------------------------------------------------- #


def test_timestamptz_text_cast_carries_session_offset(storage, session):
    q(storage, session, "CREATE TABLE tt (tstz timestamptz, ts timestamp, tz timetz)")
    q(storage, session, "SET TimeZone = 'UTC'")
    q(
        storage,
        session,
        "INSERT INTO tt VALUES ('2005-01-01 12:00:00+00', '2005-01-01 15:00:00', '15:00:00+01')",
    )
    assert q(storage, session, "SELECT tstz::text, ts::text, tz::text FROM tt").rows == [
        ("2005-01-01 12:00:00+00", "2005-01-01 15:00:00", "15:00:00+01")
    ]
    q(storage, session, "set timezone = 'gmt-3'")  # POSIX: UTC+3
    assert q(storage, session, "SELECT tstz::text FROM tt").rows == [("2005-01-01 15:00:00+03",)]


def test_timetz_text_keeps_nonzero_minutes(storage, session):
    assert q(storage, session, "SELECT '10:30:00+05:30'::timetz::text").rows == [
        ("10:30:00+05:30",)
    ]


# --------------------------------------------------------------------------- #
# BC / wide timestamptz under a session zone
# --------------------------------------------------------------------------- #


def test_bc_timestamptz_literal_takes_session_offset(storage, session):
    q(storage, session, "CREATE TABLE bt (dt timestamptz)")
    q(storage, session, "SET TimeZone = 'GMT+12'")  # POSIX: UTC-12
    q(storage, session, "INSERT INTO bt VALUES ('0101-01-01 BC')")
    assert q(storage, session, "SELECT dt::text FROM bt").rows == [
        ("0101-01-01 00:00:00-12:00 BC",)
    ]
    q(storage, session, "DELETE FROM bt")
    q(storage, session, "SET TimeZone = 'UTC'")
    q(storage, session, "INSERT INTO bt VALUES ('0101-01-01 BC')")
    assert q(storage, session, "SELECT dt::text FROM bt").rows == [("0101-01-01 00:00:00 BC",)]


# --------------------------------------------------------------------------- #
# TimeZone as a startup parameter (the pgjdbc path)
# --------------------------------------------------------------------------- #


def test_startup_timezone_parameter_applied(tmp_path):
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    try:
        host, port = srv.address
        s = socket.create_connection((host, port), timeout=5)
        try:
            s.sendall(
                pgwire.build_startup_message({"user": "joe", "database": DB, "TimeZone": "GMT+12"})
            )
            tz_status = None
            while True:
                m = pgwire.read_message(s)
                if m.type == "S":
                    name, _, value = m.payload.partition(b"\x00")
                    if name == b"TimeZone":
                        tz_status = value.rstrip(b"\x00").decode()
                if m.type == "Z":
                    break
            assert tz_status == "GMT+12"
            # The session zone really applies: a naive timestamptz literal is
            # wall clock at UTC-12, so its UTC instant is 12h later.
            s.sendall(pgwire.build_query("SELECT '1950-02-07 00:00:00'::timestamptz::text"))
            row_text = None
            while True:
                m = pgwire.read_message(s)
                if m.type == "D":
                    n = struct.unpack(">h", m.payload[:2])[0]
                    assert n == 1
                    ln = struct.unpack(">i", m.payload[2:6])[0]
                    row_text = m.payload[6 : 6 + ln].decode()
                if m.type == "Z":
                    break
            assert row_text == "1950-02-07 00:00:00-12"
        finally:
            s.close()
    finally:
        srv.stop()
        st.close()
