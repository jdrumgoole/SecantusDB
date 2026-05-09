from __future__ import annotations

import contextlib
import itertools
import logging
import socket
import threading
from types import TracebackType
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from typing import Self

from secantus.auth import ConnectionAuth
from secantus.commands import CommandContext, dispatch
from secantus.connreg import ConnectionRegistry
from secantus.cursors import CursorRegistry
from secantus.logbuf import LogBuffer
from secantus.metrics import Metrics
from secantus.sessions import SessionRegistry
from secantus.storage import Storage
from secantus.wire import (
    OP_MSG_FLAG_MORE_TO_COME,
    ConnectionClosed,
    MalformedBodyError,
    OpMsg,
    OpQuery,
    WireProtocolError,
    build_op_msg_reply,
    build_op_reply,
    read_message,
)

logger = logging.getLogger(__name__)


def _merge_op_msg_body(op: OpMsg) -> dict[str, Any]:
    body = dict(op.body)
    for identifier, docs in op.document_sequences:
        body[identifier] = docs
    return body


def _db_from_namespace(ns: str) -> str:
    db, _, _ = ns.partition(".")
    return db or "admin"


# Default per-connection idle timeout. A client that connects and
# sends nothing (or stalls mid-message) gets disconnected after this
# many seconds. Without it, a hostile peer can pin a thread forever
# by holding the socket open and not writing.
DEFAULT_CLIENT_IDLE_TIMEOUT_S = 300.0

# Default cap on simultaneous client connections. Each connection runs
# its own dedicated thread; without a cap a TCP-flood DoS exhausts the
# Python thread pool.
DEFAULT_MAX_CONNECTIONS = 1000


class SecantusDBServer:
    def __init__(
        self,
        host: str = "127.0.0.1",
        port: int = 0,
        storage_path: str = "./secantus-data",
        *,
        replica_set_name: str | None = "secantus",
        require_auth: bool = False,
        ttl_sweep_seconds: float = 60.0,
        client_idle_timeout_s: float = DEFAULT_CLIENT_IDLE_TIMEOUT_S,
        max_connections: int = DEFAULT_MAX_CONNECTIONS,
    ) -> None:
        self.host = host
        self.port = port
        self.replica_set_name = replica_set_name
        self.require_auth = require_auth
        self.client_idle_timeout_s = client_idle_timeout_s
        self.max_connections = max_connections
        self._socket: socket.socket | None = None
        self._thread: threading.Thread | None = None
        self._stop_event = threading.Event()
        # Active connection counter — incremented in _handle_client,
        # decremented on its way out. Read by _serve_forever to enforce
        # max_connections.
        self._active_conns = 0
        self._active_conns_lock = threading.Lock()
        # Oplog writes are only useful when change-stream clients can
        # connect, which requires replica-set advertisement in `hello`.
        # Skip them in pure-standalone mode to drop a per-write BSON
        # encode + oplog-table cursor write per modified document.
        self.storage = Storage(
            storage_path,
            enable_oplog=replica_set_name is not None,
            ttl_sweep_seconds=ttl_sweep_seconds,
        )
        self.cursors = CursorRegistry()
        # Per-server counters surfaced through `serverStatus`. Started
        # eagerly so `start_monotonic` reflects construction time, not
        # the first command — uptime then matches what users expect.
        self.metrics = Metrics()
        # Per-connection visibility (currentOp) and an in-memory log
        # buffer (getLog). Both initialised eagerly so the registry /
        # buffer survive the lifetime of the server, not just the first
        # connection.
        self.connections = ConnectionRegistry()
        self.logs = LogBuffer()
        # Logical-session registry: drivers send an lsid on every
        # command and bracket session lifetime via ``startSession`` /
        # ``endSessions`` / ``refreshSessions``. Tracked here so the
        # session-management commands actually have state to operate
        # on instead of returning canned ``{ok: 1}``.
        self.sessions = SessionRegistry()

    @property
    def address(self) -> tuple[str, int]:
        if self._socket is None:
            raise RuntimeError("server is not started")
        host, port, *_ = self._socket.getsockname()
        return host, port

    @property
    def uri(self) -> str:
        host, port = self.address
        return f"mongodb://{host}:{port}/"

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
            target=self._serve_forever, name="secantus-accept", daemon=True
        )
        self._thread.start()
        logger.info("secantus listening on %s:%d", self.host, self.port)

    def stop(self) -> None:
        self._stop_event.set()
        if self._socket is not None:
            with contextlib.suppress(OSError):
                self._socket.close()
            self._socket = None
        if self._thread is not None:
            self._thread.join(timeout=2.0)
            self._thread = None
        self.storage.close()

    def wait(self) -> None:
        self._stop_event.wait()

    def _serve_forever(self) -> None:
        assert self._socket is not None
        while not self._stop_event.is_set():
            try:
                conn, addr = self._socket.accept()
            except OSError:
                return
            # Refuse new connections beyond the cap. A flood-DoS attacker
            # would otherwise spawn a thread per accepted socket. We close
            # the socket immediately rather than queuing — clients should
            # see "connection refused" / EOF, not block.
            with self._active_conns_lock:
                if self._active_conns >= self.max_connections:
                    overflowed = True
                else:
                    self._active_conns += 1
                    overflowed = False
            if overflowed:
                logger.warning(
                    "rejecting connection from %s: %d active >= %d cap",
                    addr,
                    self._active_conns,
                    self.max_connections,
                )
                with contextlib.suppress(OSError):
                    conn.close()
                continue
            # Set an idle timeout on the socket so a hostile or stuck
            # peer can't pin a thread forever. Applies to all socket
            # reads in _handle_client.
            with contextlib.suppress(OSError):
                conn.settimeout(self.client_idle_timeout_s)
            handler = threading.Thread(
                target=self._handle_client,
                args=(conn, addr),
                daemon=True,
            )
            handler.start()

    def _handle_client(self, conn: socket.socket, addr: tuple[str, int]) -> None:
        connection_id = self.connections.open((addr[0], addr[1]))
        reply_ids = itertools.count(1)
        connection_auth = ConnectionAuth()
        self.metrics.connection_opened()
        logger.debug("client %d connected from %s", connection_id, addr)
        self.logs.append(
            "I", "NETWORK", "connection accepted", {"conn_id": connection_id, "from": addr}
        )
        try:
            with conn:
                while not self._stop_event.is_set():
                    try:
                        message = read_message(conn)
                    except ConnectionClosed:
                        return
                    except TimeoutError:
                        # Idle timeout fired — drop the connection so the
                        # thread can be reaped instead of pinned forever.
                        logger.debug("client %d idle timeout, closing", connection_id)
                        return
                    except MalformedBodyError as exc:
                        # Body bytes failed BSON validation. Build a
                        # targeted error reply so the client gets a
                        # ``BadValue`` and can keep using the connection
                        # for subsequent commands. Mongod returns the
                        # same shape — InvalidBSON in a command body is
                        # not a protocol-level fault.
                        logger.warning("malformed BSON from %s: %s", addr, exc)
                        try:
                            reply = build_op_msg_reply(
                                response_to=exc.header.request_id,
                                request_id=next(reply_ids),
                                body={
                                    "ok": 0.0,
                                    "errmsg": str(exc),
                                    "code": 2,
                                    "codeName": "BadValue",
                                },
                            )
                            conn.sendall(reply)
                        except OSError:
                            # Client may have hung up before we could
                            # respond; fall through to the close path.
                            return
                        continue
                    except WireProtocolError as exc:
                        logger.warning("wire protocol error from %s: %s", addr, exc)
                        return

                    op = message.op
                    try:
                        server_addr = self.address if self._socket is not None else None
                    except RuntimeError:
                        server_addr = None
                    if isinstance(op, OpMsg):
                        body = _merge_op_msg_body(op)
                        ctx = CommandContext(
                            connection_id=connection_id,
                            storage=self.storage,
                            cursors=self.cursors,
                            db_name=body.get("$db", "admin"),
                            server_address=server_addr,
                            replica_set_name=self.replica_set_name,
                            connection_auth=connection_auth,
                            require_auth=self.require_auth,
                            metrics=self.metrics,
                            connections=self.connections,
                            logs=self.logs,
                            sessions=self.sessions,
                        )
                        response_doc = dispatch(body, ctx)
                        # `moreToCome` (bit 1) is the wire signal for
                        # fire-and-forget requests — `writeConcern: {w: 0}`
                        # unacknowledged writes use it. The spec requires the
                        # server NOT to send a reply; if we do, the driver
                        # mis-pairs it with the next response and aborts the
                        # connection with a responseTo/requestId mismatch.
                        if op.flags & OP_MSG_FLAG_MORE_TO_COME:
                            continue
                        reply = build_op_msg_reply(
                            response_to=message.header.request_id,
                            request_id=next(reply_ids),
                            body=response_doc,
                        )
                    elif isinstance(op, OpQuery):
                        ctx = CommandContext(
                            connection_id=connection_id,
                            storage=self.storage,
                            cursors=self.cursors,
                            db_name=_db_from_namespace(op.full_collection_name),
                            server_address=server_addr,
                            replica_set_name=self.replica_set_name,
                            connection_auth=connection_auth,
                            require_auth=self.require_auth,
                            metrics=self.metrics,
                            connections=self.connections,
                            logs=self.logs,
                            sessions=self.sessions,
                        )
                        response_doc = dispatch(op.query, ctx)
                        reply = build_op_reply(
                            response_to=message.header.request_id,
                            request_id=next(reply_ids),
                            documents=[response_doc],
                        )
                    else:
                        logger.warning("unhandled op type %r from %s", type(op), addr)
                        return

                    try:
                        conn.sendall(reply)
                    except OSError:
                        return
        except Exception:
            logger.exception("unhandled error on connection %d", connection_id)
        finally:
            self.metrics.connection_closed()
            self.connections.close(connection_id)
            with self._active_conns_lock:
                self._active_conns -= 1
            logger.debug("client %d disconnected", connection_id)

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
