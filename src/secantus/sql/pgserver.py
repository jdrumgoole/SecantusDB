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

import contextlib
import logging
import os
import secrets
import socket
import ssl
import threading
from types import TracebackType
from typing import TYPE_CHECKING, Any

from secantus.sql import errors, pgwire, typemap
from secantus.sql.engine import run_sql
from secantus.sql.pgauth import (
    SCRAM_SHA_256,
    PGAuthError,
    ScramExchange,
    UserStore,
    mock_credentials,
)
from secantus.sql.pgextended import ExtendedSession
from secantus.sql.session import SERVER_VERSION, Session

if TYPE_CHECKING:
    from typing import Self

logger = logging.getLogger(__name__)

DEFAULT_CLIENT_IDLE_TIMEOUT_S = 300.0


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
        require_auth: bool = False,
        users: dict[str, str] | None = None,
        tls_cert_file: str | None = None,
        tls_key_file: str | None = None,
    ) -> None:
        self.host = host
        self.port = port
        self.default_database = default_database
        self.client_idle_timeout_s = client_idle_timeout_s
        self._socket: socket.socket | None = None
        self._thread: threading.Thread | None = None
        self._stop_event = threading.Event()
        self._conns: set[socket.socket] = set()
        self._conns_lock = threading.Lock()
        # SCRAM-SHA-256 auth: when require_auth is on, clients must authenticate
        # against a user from ``users`` (username -> plaintext, hashed into a
        # SCRAM verifier at startup; the plaintext is not retained).
        self.require_auth = require_auth
        self._users = UserStore.from_passwords(users or {})
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
            with contextlib.suppress(OSError):
                conn.settimeout(self.client_idle_timeout_s)
            threading.Thread(target=self._handle_client, args=(conn, addr), daemon=True).start()

    # -- per-connection ----------------------------------------------------- #

    def _handle_client(self, conn: socket.socket, addr: tuple[str, int]) -> None:
        with self._conns_lock:
            self._conns.add(conn)
        try:
            with conn:
                result = self._handshake(conn)
                if result is None:
                    return
                io, session = result
                self._query_loop(io, session)
        except (pgwire.PGConnectionClosed, ConnectionError, TimeoutError, ssl.SSLError, OSError):
            return
        except Exception:
            logger.exception("unhandled error on pg connection from %s", addr)
        finally:
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
        backend_pid = os.getpid() & 0x7FFFFFFF

        if self.require_auth and not self._authenticate(io, user):
            return None

        session = Session(database=db, user=user, backend_pid=backend_pid)
        if application_name:
            session.settings["application_name"] = application_name

        out = bytearray()
        out += pgwire.authentication_ok()
        for name, value in (
            ("server_version", SERVER_VERSION),
            ("server_encoding", "UTF8"),
            ("client_encoding", "UTF8"),
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
            msg = pgwire.read_message(conn)
            if msg.type == "X":  # Terminate
                return
            if msg.type == "Q":  # simple Query
                self._handle_query(conn, session, pgwire.parse_query(msg.payload))
                continue
            # Everything else is the extended query protocol
            # (Parse/Bind/Describe/Execute/Close/Sync/Flush).
            reply = ext.process(msg.type, msg.payload)
            if reply:
                conn.sendall(reply)

    def _handle_query(self, conn: socket.socket, session: Session, sql: str) -> None:
        out = bytearray()
        try:
            results = run_sql(self.storage, session.database, sql, session=session)
            if not results:
                out += pgwire.empty_query_response()
            else:
                for res in results:
                    out += _render_result(res)
        except errors.SQLError as exc:
            out += pgwire.error_response(exc.sqlstate, exc.message)
        except Exception as exc:  # pragma: no cover - defensive
            logger.exception("error executing SQL")
            out += pgwire.error_response("XX000", f"internal error: {exc}")
        out += pgwire.ready_for_query(b"I")
        conn.sendall(bytes(out))

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


def _render_result(res: Any) -> bytes:
    """Serialise one ``SQLResult`` to its backend messages."""
    out = bytearray()
    for name, value in res.parameter_status:
        out += pgwire.parameter_status(name, value)
    if res.columns or res.command_tag.startswith("SELECT"):
        out += pgwire.row_description([(c.name, c.pg_oid) for c in res.columns])
        for row in res.rows:
            out += pgwire.data_row([typemap.to_pg_text(v) for v in row])
    out += pgwire.command_complete(res.command_tag)
    return bytes(out)
