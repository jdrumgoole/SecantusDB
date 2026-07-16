"""``SecantusPGServer`` — a PostgreSQL-wire front end over the document store.

The SQL analogue of ``SecantusDBServer``: a TCP accept loop with one daemon
thread per connection. Each connection does the Postgres startup handshake
(trust auth in P1), then runs simple ``Query`` messages through
``secantus.sql.run_sql`` against the shared ``Storage`` and streams the rows
back as ``RowDescription`` / ``DataRow`` / ``CommandComplete``.

P1 scope: ``SSLRequest`` → no-TLS, startup, trust auth, the simple query
protocol. TLS, SCRAM auth, and the extended query protocol are later phases.
The server shares the *same* ``Storage`` the Mongo server uses, so it can run
standalone or alongside ``SecantusDBServer`` on a second port — the
dual-protocol view.
"""

from __future__ import annotations

import argparse
import contextlib
import datetime as _dt
import itertools
import logging
import os
import secrets
import signal
import socket
import ssl
import sys
import threading
import time
from types import FrameType, TracebackType
from typing import TYPE_CHECKING, Any

from secantus.sql import copyfmt, errors, pgwire, planner, typemap
from secantus.sql import engine as sql_engine
from secantus.sql import session as sql_session
from secantus.sql.catalog import Catalog
from secantus.sql.engine import run_sql
from secantus.sql.pgauth import (
    SCRAM_SHA_256,
    PGAuthError,
    ScramExchange,
    UserStore,
    mock_credentials,
)
from secantus.sql.pgextended import ExtendedSession
from secantus.sql.pgnotify import NotifyHub
from secantus.sql.session import (
    SERVER_VERSION,
    ActivityRegistry,
    PreparedXactRegistry,
    Session,
)

if TYPE_CHECKING:
    from typing import Self

logger = logging.getLogger(__name__)

DEFAULT_CLIENT_IDLE_TIMEOUT_S = 300.0
#: Cap on concurrently-served connections — an over-cap accept is closed
#: immediately rather than spawning a thread (mirrors the Mongo server's
#: ``DEFAULT_MAX_CONNECTIONS``). Bounds the fan-out of the per-connection cursor
#: caps so total memory is bounded even under an unauthenticated flood. (#194)
DEFAULT_MAX_CONNECTIONS = 1000

#: Wire message for an unexpected internal error. The raw Python exception text
#: is written to the server log (``logger.exception``) but never sent to the
#: client — leaking it could disclose internal paths, types, or data values.
#: Mirrors the Mongo dispatch's generic-error discipline. (security review §I17)
_INTERNAL_ERROR_MSG = "internal error"


class SecantusPGServer:
    def __init__(
        self,
        host: str = "127.0.0.1",
        port: int = 0,
        storage_path: str = "./secantus-pg-data",
        *,
        storage: Any = None,
        default_database: str = "postgres",
        client_idle_timeout_s: float = DEFAULT_CLIENT_IDLE_TIMEOUT_S,
        max_connections: int = DEFAULT_MAX_CONNECTIONS,
        require_auth: bool = False,
        users: dict[str, str] | None = None,
        user_roles: dict[str, list[dict[str, str]]] | None = None,
        tls_cert_file: str | None = None,
        tls_key_file: str | None = None,
    ) -> None:
        self.host = host
        self.port = port
        self.default_database = default_database
        self.client_idle_timeout_s = client_idle_timeout_s
        self.max_connections = max_connections
        self._socket: socket.socket | None = None
        self._thread: threading.Thread | None = None
        self._stop_event = threading.Event()
        self._conns: set[socket.socket] = set()
        self._conns_lock = threading.Lock()
        # Live handler threads, joined by ``stop()`` (pruned of dead threads on
        # each accept so the set stays bounded by the connection cap).
        self._handler_threads: set[threading.Thread] = set()
        # Server-wide LISTEN / NOTIFY channel registry.
        self._notify = NotifyHub()
        # Server-wide live-session registry for pg_stat_activity (#137), plus a
        # monotonic per-connection backend pid (real Postgres gives each backend a
        # distinct pid; in-process we'd otherwise share os.getpid()).
        self._activity = ActivityRegistry()
        # Server-wide prepared two-phase transactions (#139), shared by all
        # connections so COMMIT PREPARED / ROLLBACK PREPARED can resolve a gid
        # prepared on a different connection.
        self._prepared_xacts = PreparedXactRegistry()
        # itertools.count.__next__ is atomic under the GIL, so no extra lock.
        self._backend_pid_seq = itertools.count((os.getpid() & 0x7FFFFF) << 8 | 1)
        # SCRAM-SHA-256 auth: when require_auth is on, clients must authenticate
        # against a user from ``users`` (username -> plaintext, hashed into a
        # SCRAM verifier at startup; the plaintext is not retained).
        self.require_auth = require_auth
        self._users = UserStore.from_passwords(users or {}, user_roles)
        # Per-statement RBAC is enforced only when explicit per-user role bindings
        # are supplied *and* auth is on (an identity is required to authorize).
        # Without ``user_roles`` the SQL surface stays unrestricted — the
        # documented trust default, unchanged from prior behaviour. When active,
        # statements are gated by ``secantus.rbac.check_privilege`` against the
        # authenticated user's roles, the same model the Mongo server uses. (#193)
        self._authz_active = bool(require_auth and user_roles)
        # Optional TLS: when a cert/key pair is given, an SSLRequest is answered
        # 'S' and the socket is wrapped before the startup flow. Without it, the
        # server declines TLS ('N') and stays plaintext.
        if (tls_cert_file is None) != (tls_key_file is None):
            raise ValueError("tls_cert_file and tls_key_file must both be set or both be None")
        self._ssl_context: ssl.SSLContext | None = None
        if tls_cert_file is not None and tls_key_file is not None:
            ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            ctx.load_cert_chain(certfile=tls_cert_file, keyfile=tls_key_file)
            self._ssl_context = ctx
        # Storage is injectable so the server is testable without a WiredTiger
        # build; when not provided we create the real WT-backed store lazily
        # (the import is deferred so importing this module never needs WT).
        if storage is None:
            from secantus.storage import Storage

            self.storage = Storage(storage_path)
            self._owns_storage = True
        else:
            self.storage = storage
            self._owns_storage = False

    # -- lifecycle ---------------------------------------------------------- #

    @property
    def address(self) -> tuple[str, int]:
        if self._socket is None:
            raise RuntimeError("server is not started")
        host, port, *_ = self._socket.getsockname()
        return host, port

    @property
    def uri(self) -> str:
        host, port = self.address
        return f"postgresql://{host}:{port}/{self.default_database}"

    def start(self) -> None:
        if self._socket is not None:
            raise RuntimeError("server is already started")
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.bind((self.host, self.port))
        sock.listen()
        self._socket = sock
        self.host, self.port = sock.getsockname()[:2]
        self._stop_event.clear()
        self._thread = threading.Thread(
            target=self._serve_forever, name="secantus-pg-accept", daemon=True
        )
        self._thread.start()
        logger.info("secantus pg listening on %s:%d", self.host, self.port)

    def stop(self) -> None:
        self._stop_event.set()
        if self._socket is not None:
            with contextlib.suppress(OSError):
                self._socket.close()
            self._socket = None
        with self._conns_lock:
            conns = list(self._conns)
        for c in conns:
            with contextlib.suppress(OSError):
                c.shutdown(socket.SHUT_RDWR)
            with contextlib.suppress(OSError):
                c.close()
        if self._thread is not None:
            self._thread.join(timeout=2.0)
            self._thread = None
        # Drain handler threads before returning: a handler mid-request may
        # still be using its per-thread WT session, and an embedder is entitled
        # to call ``storage.close()`` right after ``stop()`` — closing a WT
        # session another thread is concurrently using corrupts the session
        # handle (surfaces as a ``Session__freecb`` TypeError during close).
        with self._conns_lock:
            handlers = [t for t in self._handler_threads if t.is_alive()]
            self._handler_threads.clear()
        deadline = time.monotonic() + 5.0
        for t in handlers:
            t.join(timeout=max(0.0, deadline - time.monotonic()))
            if t.is_alive():
                logger.warning("pg handler thread %s still alive after stop()", t.name)
        if self._owns_storage:
            with contextlib.suppress(Exception):
                self.storage.close()

    def _serve_forever(self) -> None:
        assert self._socket is not None
        while not self._stop_event.is_set():
            try:
                conn, addr = self._socket.accept()
            except OSError:
                return
            # Enforce the connection cap before spawning a handler, and register
            # the socket under the lock so the count is accurate (the handler
            # removes it on exit). An over-cap accept is closed immediately — the
            # client sees a reset, not a hang. (#194)
            with self._conns_lock:
                if len(self._conns) >= self.max_connections:
                    over_cap = True
                else:
                    self._conns.add(conn)
                    over_cap = False
            if over_cap:
                logger.warning(
                    "rejecting PG connection from %s: at %d-connection cap",
                    addr,
                    self.max_connections,
                )
                with contextlib.suppress(OSError):
                    conn.close()
                continue
            with contextlib.suppress(OSError):
                conn.settimeout(self.client_idle_timeout_s)
            handler = threading.Thread(target=self._handle_client, args=(conn, addr), daemon=True)
            with self._conns_lock:
                self._handler_threads = {t for t in self._handler_threads if t.is_alive()}
                self._handler_threads.add(handler)
            handler.start()

    # -- per-connection ----------------------------------------------------- #

    def _handle_client(self, conn: socket.socket, addr: tuple[str, int]) -> None:
        # NB: the socket is already registered in ``self._conns`` by
        # ``_serve_forever`` (under the cap check); we only remove it on exit.
        session: Session | None = None
        try:
            with conn:
                result = self._handshake(conn)
                if result is None:
                    return
                io, session = result
                session.client_addr = addr[0] if addr else None
                self._activity.register(session)  # pg_stat_activity (#137)
                self._query_loop(io, session)
        except (pgwire.PGConnectionClosed, ConnectionError, TimeoutError, ssl.SSLError, OSError):
            return
        except Exception:
            logger.exception("unhandled error on pg connection from %s", addr)
        finally:
            # A connection that drops mid-transaction must not leak the open
            # Storage transaction (real WT session); roll it back.
            if session is not None and session.txn_handle is not None:
                with contextlib.suppress(Exception):
                    self.storage.abort_user_transaction(session.txn_handle)
            if session is not None:
                self._notify.unlisten_all(session)  # drop this conn's LISTENs
                self._activity.unregister(session)  # drop from pg_stat_activity (#137)
            # Release this thread's cached WT session + cursors back to the
            # engine, exactly like the Mongo server's teardown (server.py).
            # Without this every pg connection leaked its WT session: the
            # dead thread's positioned cursors kept pages pinned, and after a
            # few hundred connections cache eviction livelocked with an
            # application thread stuck in __wt_cache_eviction_worker while
            # holding the storage RLock — every other connection then queued
            # forever (the psycopg gauge's single-daemon run wedged at
            # ~test 420, three out of three runs).
            with contextlib.suppress(Exception):
                self.storage._reset_thread_session()
            with self._conns_lock:
                self._conns.discard(conn)

    def _handshake(self, conn: socket.socket) -> tuple[socket.socket, Session] | None:
        """Negotiate TLS (if requested) + startup + auth.

        Returns ``(io_socket, session)`` — ``io_socket`` is the TLS-wrapped
        socket when TLS was negotiated, else ``conn`` — or None to drop.
        """
        io = conn
        # SSL/GSSENC requests precede the StartupMessage. We answer 'S' and wrap
        # when TLS is configured, otherwise decline ('N') and read again. A
        # CancelRequest on a fresh socket is a no-op drop.
        while True:
            packet = pgwire.read_startup_packet(io)
            if isinstance(packet, pgwire.SSLRequest) and self._ssl_context is not None:
                io.sendall(b"S")
                io = self._ssl_context.wrap_socket(io, server_side=True)
                continue
            if isinstance(packet, pgwire.SSLRequest | pgwire.GSSENCRequest):
                io.sendall(b"N")
                continue
            if isinstance(packet, pgwire.CancelRequest):
                return None
            startup = packet
            break

        db = startup.params.get("database") or startup.params.get("user") or self.default_database
        user = startup.params.get("user", "secantus")
        application_name = startup.params.get("application_name", "")
        backend_pid = next(self._backend_pid_seq) & 0x7FFFFFFF

        if self.require_auth and not self._authenticate(io, user):
            return None

        session = Session(database=db, user=user, backend_pid=backend_pid)
        # Bind the session to this connection thread's render context so
        # to_pg_text can honour per-session GUCs (TimeZone) at output time.
        typemap.set_render_session(session)
        session.notify_hub = self._notify
        session.activity_registry = self._activity
        session.prepared_xacts = self._prepared_xacts
        session.backend_start = _dt.datetime.now(_dt.timezone.utc)
        if self._authz_active:
            session.authz_active = True
            session.roles = self._users.roles_for(user)
        if application_name:
            session.settings["application_name"] = application_name
        # client_encoding may arrive as a startup parameter; honour it the same
        # way SET client_encoding would (unknown values fall back to UTF8 —
        # erroring here would abort the handshake).
        startup_enc = startup.params.get("client_encoding")
        if startup_enc:
            canonical = sql_session.canonical_client_encoding(startup_enc)
            if canonical is not None:
                session.settings["client_encoding"] = canonical

        out = bytearray()
        out += pgwire.authentication_ok()
        for name, value in (
            ("server_version", SERVER_VERSION),
            ("server_encoding", "UTF8"),
            ("client_encoding", session.get_setting("client_encoding")),
            ("DateStyle", "ISO, MDY"),
            ("integer_datetimes", "on"),
            ("standard_conforming_strings", "on"),
            ("TimeZone", "UTC"),
            ("application_name", application_name),
            ("is_superuser", "off"),
            ("session_authorization", user),
        ):
            out += pgwire.parameter_status(name, value)
        # A nominal pid/secret so CancelRequest has something to echo (cancel
        # isn't honoured in P1, but clients store these).
        out += pgwire.backend_key_data(backend_pid, secrets.randbits(31))
        out += pgwire.ready_for_query(b"I")
        io.sendall(bytes(out))
        return io, session

    def _authenticate(self, io: socket.socket, user: str) -> bool:
        """Run the SCRAM-SHA-256 exchange. Returns True on success.

        On failure an ErrorResponse (SQLSTATE 28P01) is sent so the client
        surfaces "password authentication failed" rather than a bare hang-up.
        """
        # Unknown user → a throwaway verifier so the exchange fails at the same
        # step as a wrong password (no username enumeration via the handshake).
        creds = self._users.get(user) or mock_credentials()
        io.sendall(pgwire.authentication_sasl([SCRAM_SHA_256]))
        try:
            msg = pgwire.read_message(io)
            mech, client_first = pgwire.parse_sasl_initial_response(msg.payload)
            if msg.type != "p" or mech != SCRAM_SHA_256:
                raise PGAuthError("authentication failed")
            exch = ScramExchange(creds)
            io.sendall(pgwire.authentication_sasl_continue(exch.server_first(client_first)))
            final_msg = pgwire.read_message(io)
            if final_msg.type != "p":
                raise PGAuthError("expected SASLResponse")
            server_final = exch.server_final(pgwire.parse_sasl_response(final_msg.payload))
            io.sendall(pgwire.authentication_sasl_final(server_final))
            return True
        except PGAuthError:
            io.sendall(
                pgwire.error_response("28P01", f'password authentication failed for user "{user}"')
            )
            return False

    def _query_loop(self, conn: socket.socket, session: Session) -> None:
        # Per-connection extended-protocol state (prepared statements + portals).
        ext = ExtendedSession(self.storage, session)
        while not self._stop_event.is_set():
            try:
                msg = pgwire.read_message(conn)
            except pgwire.PGProtocolError:
                # A framing error (implausible length) desyncs the byte stream —
                # unrecoverable. Send a FATAL protocol-violation ErrorResponse
                # so the client sees a reason, then close, rather than silently
                # dropping the connection. (§I16)
                logger.warning("malformed PG message framing from client; closing")
                with contextlib.suppress(OSError):
                    conn.sendall(pgwire.error_response("08P01", "protocol violation"))
                return
            if msg.type == "X":  # Terminate
                return
            if msg.type == "Q":  # simple Query
                try:
                    sql = pgwire.parse_query(msg.payload, session.wire_encoding)
                except UnicodeDecodeError:
                    # The message was fully length-framed and read, so the byte
                    # stream stays in sync — report the bad message and keep
                    # serving instead of dropping the connection. (§I16)
                    conn.sendall(
                        pgwire.error_response(
                            "08P01",
                            "invalid byte sequence for client_encoding in query message",
                            encoding=session.wire_encoding,
                        )
                        + pgwire.ready_for_query(session.txn_status())
                    )
                    continue
                self._handle_query(conn, session, sql)
                continue
            # Everything else is the extended query protocol
            # (Parse/Bind/Describe/Execute/Close/Sync/Flush). Async LISTEN/NOTIFY
            # deliveries ride out ahead of the reply (message boundaries).
            reply = ext.process(msg.type, msg.payload)
            notifications = self._pending_notification_bytes(session)
            if notifications or reply:
                conn.sendall(notifications + (reply or b""))

    def _pending_notification_bytes(self, session: Session) -> bytes:
        """Serialize this connection's queued async LISTEN/NOTIFY deliveries into
        ``NotificationResponse`` bytes to write on the owning thread (so socket
        writes stay serialized). Delivered inline with the query cycle — right
        before a ``ReadyForQuery`` — rather than via a background poll (which would
        busy-wake every idle connection)."""
        out = bytearray()
        for pid, channel, payload in session.drain_notifications():
            out += pgwire.notification_response(pid, channel, payload)
        return bytes(out)

    def _handle_query(self, conn: socket.socket, session: Session, sql: str) -> None:
        # A COPY … FROM/TO STDIN/STDOUT statement drives its own sub-protocol
        # (CopyIn/CopyOut) mid-query, so it can't go through run_sql.
        from sqlglot import exp

        # pg_stat_activity (#137): this backend is 'active' with ``sql`` while the
        # query runs; it stays as the last query (state 'idle') afterwards.
        session.state = "active"
        session.current_query = sql
        session.query_start = _dt.datetime.now(_dt.timezone.utc)
        out = bytearray()
        try:
            # planner.parse must run inside the try: a syntax error raises a
            # SQLError (42601), and if that escaped it would drop the connection
            # with no ErrorResponse rather than reporting the error. (§I16)
            # LISTEN/NOTIFY/UNLISTEN bypass the COPY probe — sqlglot mis-parses
            # them (and errors on a NOTIFY payload), so run_sql handles them.
            # Two-phase commit (#139) also bypasses the probe: sqlglot can't parse
            # COMMIT/ROLLBACK PREPARED, so run_sql intercepts them pre-parse.
            if not sql_engine.is_pubsub_statement(sql) and not sql_engine.is_two_phase_statement(
                sql
            ):
                stmts = planner.parse(sql)
                if len(stmts) == 1 and isinstance(stmts[0], exp.Copy):
                    self._handle_copy(conn, session, stmts[0])
                    return
            results = run_sql(self.storage, session.database, sql, session=session)
            if not results:
                out += pgwire.empty_query_response()
            else:
                for res in results:
                    out += _render_result(res, session.wire_encoding, session)
        except errors.SQLError as exc:
            out += pgwire.error_response(exc.sqlstate, exc.message, encoding=session.wire_encoding)
        except Exception:  # pragma: no cover - defensive
            logger.exception("error executing SQL")
            # Don't leak the raw Python exception text to the wire client — the
            # full detail is in the server log. Mirrors the Mongo dispatch's
            # generic-error discipline. (security review 2026-07-04 §I17)
            out += pgwire.error_response("XX000", _INTERNAL_ERROR_MSG)
        # Deliver any async LISTEN/NOTIFY notifications (e.g. this query's own
        # NOTIFY, or one from another connection) before ReadyForQuery, matching
        # Postgres, so the client picks them up in this same query exchange.
        out += self._pending_notification_bytes(session)
        # The ReadyForQuery status reflects the transaction block (I/T/E).
        out += pgwire.ready_for_query(session.txn_status())
        session.state = "idle"  # query done; pg_stat_activity shows it idle (#137)
        conn.sendall(bytes(out))

    def _txn_scope(self, session: Session) -> Any:
        """The session's open user transaction as a context manager, or a
        no-op scope outside a BEGIN block. COPY's catalog resolution, reads,
        and writes must all run inside the transaction — otherwise a COPY
        can't see same-transaction DDL (``CREATE TABLE`` + ``COPY`` in one
        block is psycopg's standard fixture shape) and, worse, its rows land
        OUTSIDE the transaction and would survive a ROLLBACK."""
        if session.txn_handle is not None:
            return self.storage.use_user_transaction(session.txn_handle)
        return contextlib.nullcontext()

    def _handle_copy(self, conn: socket.socket, session: Session, stmt: Any) -> None:
        """Run the ``COPY`` sub-protocol: CopyInResponse → CopyData* → CopyDone for
        ``FROM STDIN``, or CopyOutResponse → CopyData* → CopyDone for ``TO STDOUT``."""
        catalog = Catalog(self.storage)
        try:
            with self._txn_scope(session):
                plan = sql_engine.copy_plan(stmt, self.storage, session.database, catalog, session)
            if plan.to_stdout:
                self._copy_out(conn, session, catalog, plan)
            else:
                self._copy_in(conn, session, catalog, plan)
        except errors.SQLError as exc:
            if session.txn_handle is not None:
                session.txn_failed = True  # a failed COPY aborts the block, like PG
            conn.sendall(pgwire.error_response(exc.sqlstate, exc.message))
            conn.sendall(pgwire.ready_for_query(session.txn_status()))
        except Exception:  # pragma: no cover - defensive
            logger.exception("error executing COPY")
            if session.txn_handle is not None:
                session.txn_failed = True
            # Generic wire message; full detail stays in the server log. (§I17)
            conn.sendall(pgwire.error_response("XX000", _INTERNAL_ERROR_MSG))
            conn.sendall(pgwire.ready_for_query(session.txn_status()))

    def _copy_in(self, conn: socket.socket, session: Session, catalog: Any, plan: Any) -> None:
        conn.sendall(pgwire.copy_in_response(len(plan.columns)))
        chunks: list[bytes] = []
        while True:
            msg = pgwire.read_message(conn)
            if msg.type == "d":  # CopyData
                chunks.append(msg.payload)
            elif msg.type == "c":  # CopyDone
                break
            elif msg.type == "f":  # CopyFail
                reason = msg.payload.split(b"\x00", 1)[0].decode("utf-8", "replace")
                if session.txn_handle is not None:
                    # A failed COPY aborts the enclosing transaction (INERROR),
                    # exactly like any other errored statement.
                    session.txn_failed = True
                conn.sendall(pgwire.error_response("57014", f"COPY from stdin failed: {reason}"))
                conn.sendall(pgwire.ready_for_query(session.txn_status()))
                return
            else:  # pragma: no cover - client desync
                break
        try:
            data = pgwire.decode_text(b"".join(chunks), session.wire_encoding)
        except UnicodeDecodeError:
            # Binary/garbage COPY payloads must surface as a faithful SQL
            # error, not an internal one.
            raise errors.SQLError(
                "22021", f'invalid byte sequence for encoding "{session.wire_encoding}"'
            ) from None
        if plan.fmt == "csv":
            rows = copyfmt.parse_csv(
                data, delimiter=plan.delimiter, null=plan.null, header=plan.header
            )
        else:
            rows = copyfmt.parse_text(data, delimiter=plan.delimiter, null=plan.null)
        try:
            with self._txn_scope(session):
                n = sql_engine.copy_insert(
                    self.storage, session.database, catalog, session, plan, rows
                )
        except errors.SQLError as exc:
            if session.txn_handle is not None:
                session.txn_failed = True
            conn.sendall(pgwire.error_response(exc.sqlstate, exc.message))
            conn.sendall(pgwire.ready_for_query(session.txn_status()))
            return
        conn.sendall(pgwire.command_complete(f"COPY {n}"))
        conn.sendall(pgwire.ready_for_query(session.txn_status()))

    def _copy_out(self, conn: socket.socket, session: Session, catalog: Any, plan: Any) -> None:
        with self._txn_scope(session):
            rows = sql_engine.copy_extract(self.storage, session.database, catalog, session, plan)
        conn.sendall(pgwire.copy_out_response(len(plan.columns)))
        # One CopyData message per logical row, like a real server — libpq
        # clients (psycopg's Copy.rows()) frame rows by message. Each row is
        # FORMATTED individually: splitting a pre-rendered blob on newlines
        # would splinter CSV rows with quoted embedded newlines, and
        # str.splitlines would additionally split on U+0085/U+2028-style
        # separators inside escaped text-format fields.
        chunks: list[str] = []
        if plan.fmt == "csv":
            if plan.header:
                chunks.append(
                    copyfmt.format_csv(
                        [], delimiter=plan.delimiter, null=plan.null, header=plan.columns
                    )
                )
            chunks += [
                copyfmt.format_csv([row], delimiter=plan.delimiter, null=plan.null) for row in rows
            ]
        else:
            chunks += [
                copyfmt.format_text([row], delimiter=plan.delimiter, null=plan.null) for row in rows
            ]
        if chunks:
            out = bytearray()
            for chunk in chunks:
                if chunk:
                    out += pgwire.copy_data(pgwire.encode_text(chunk, session.wire_encoding))
            conn.sendall(bytes(out))
        conn.sendall(pgwire.copy_done())
        conn.sendall(pgwire.command_complete(f"COPY {len(rows)}"))
        conn.sendall(pgwire.ready_for_query(session.txn_status()))

    # -- context manager ---------------------------------------------------- #

    def __enter__(self) -> Self:
        self.start()
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        self.stop()


def _render_result(res: Any, encoding: str | None = "utf-8", session: Any = None) -> bytes:
    """Serialise one ``SQLResult`` to its backend messages."""
    out = bytearray()
    status = list(res.parameter_status)
    if session is not None and session.pending_parameter_status:
        # Reportable GUCs changed mid-statement by set_config().
        status += session.pending_parameter_status
        session.pending_parameter_status = []
    for name, value in status:
        out += pgwire.parameter_status(name, value)
    if res.columns or res.command_tag.startswith("SELECT"):
        out += pgwire.row_description([(c.name, c.pg_oid) for c in res.columns], encoding=encoding)
        tags = [c.type_tag for c in res.columns]
        for row in res.rows:
            out += pgwire.data_row(
                [
                    pgwire.transcode_out(typemap.to_pg_text(v, t), encoding)
                    for v, t in zip(row, tags, strict=False)
                ]
            )
    out += pgwire.command_complete(res.command_tag)
    return bytes(out)


def build_parser() -> argparse.ArgumentParser:
    """Argparse parser for the ``secantusd-py-pg`` daemon."""
    parser = argparse.ArgumentParser(
        prog="secantusd-py-pg",
        description=(
            "Run a SecantusDB standalone single-node server speaking the "
            "PostgreSQL wire protocol over the shared document store."
        ),
    )
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=5432)
    parser.add_argument(
        "--storage-path",
        default="./secantus-pg-data",
        metavar="PATH",
        help=(
            "WiredTiger home directory (default: './secantus-pg-data'). Created "
            "if missing; reopened intact across restarts."
        ),
    )
    parser.add_argument(
        "--default-database",
        default="postgres",
        metavar="NAME",
        help="Database reported to clients that don't request one (default: postgres).",
    )
    parser.add_argument(
        "--log-level",
        default="INFO",
        choices=["DEBUG", "INFO", "WARNING", "ERROR"],
    )
    parser.add_argument(
        "--tls-cert-file",
        default=None,
        metavar="PATH",
        help=(
            "PEM-format server certificate chain. When this and --tls-key-file "
            "are both set, an SSLRequest is answered and the socket is TLS-"
            "wrapped before the startup flow; without them the server stays "
            "plaintext."
        ),
    )
    parser.add_argument(
        "--tls-key-file",
        default=None,
        metavar="PATH",
        help="PEM-format private key matching --tls-cert-file.",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)

    logging.basicConfig(
        level=args.log_level,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )

    server = SecantusPGServer(
        host=args.host,
        port=args.port,
        storage_path=args.storage_path,
        default_database=args.default_database,
        tls_cert_file=args.tls_cert_file,
        tls_key_file=args.tls_key_file,
    )

    stopped = threading.Event()

    def handle_signal(signum: int, frame: FrameType | None) -> None:
        server.stop()
        stopped.set()

    signal.signal(signal.SIGINT, handle_signal)
    signal.signal(signal.SIGTERM, handle_signal)

    server.start()
    stopped.wait()
    return 0


if __name__ == "__main__":
    sys.exit(main())
