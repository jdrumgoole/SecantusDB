from __future__ import annotations

import contextlib
import itertools
import logging
import select
import socket
import ssl
import sys
import threading
import time
import traceback
from types import TracebackType
from typing import TYPE_CHECKING, Any

import bson

if TYPE_CHECKING:
    from typing import Self

from secantus.auth import ConnectionAuth, subject_dn_from_peercert
from secantus.commands import CommandContext, dispatch
from secantus.connreg import ConnectionRegistry
from secantus.cursors import CursorRegistry
from secantus.failpoints import CloseConnectionRequested, FailPointRegistry
from secantus.logbuf import LogBuffer
from secantus.metrics import Metrics
from secantus.sessions import SessionRegistry
from secantus.storage import Storage
from secantus.transactions import Transaction, TransactionRegistry
from secantus.wire import (
    OP_MSG_FLAG_EXHAUST_ALLOWED,
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


def _tune_client_socket(conn: socket.socket) -> None:
    """Disable Nagle on an accepted client socket. Reply paths write small
    frames back-to-back (a reply then ReadyForQuery, one batch item's result
    then the next); with Nagle on, the second write waits for the peer's
    delayed ACK — ~40ms per round trip on Linux, invisible on macOS loopback.
    pgjdbc's generated-keys batches (1000 single-row round trips per test)
    measured 41.5s per test in CI against 0.2s locally from exactly this.
    Real servers (mongod, PostgreSQL) set TCP_NODELAY unconditionally."""
    with contextlib.suppress(OSError):
        conn.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)


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
        noop_heartbeat_seconds: float = 0.0,
        client_idle_timeout_s: float = DEFAULT_CLIENT_IDLE_TIMEOUT_S,
        max_connections: int = DEFAULT_MAX_CONNECTIONS,
        oplog_retention_seconds: float = 3600.0,
        oplog_max_entries: int = 100_000,
        oplog_archive_dir: str | None = None,
        cache_size: str = "1G",
        session_max: int = 1000,
        sync_on_commit: bool = False,
        durable: bool | None = None,
        transaction_lifetime_seconds: float = 60.0,
        tls_cert_file: str | None = None,
        tls_key_file: str | None = None,
        tls_ca_file: str | None = None,
        tls_require_client_cert: bool = False,
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
        # TLS: when both cert + key files are provided, accept()ed sockets
        # are wrapped with an SSLContext before being handed off to the
        # connection thread. Clients then negotiate TLS first, and only
        # then start sending mongo wire frames over the encrypted channel.
        # Without TLS the daemon stays plaintext as it always has.
        if (tls_cert_file is None) != (tls_key_file is None):
            raise ValueError("tls_cert_file and tls_key_file must both be set or both be None")
        # mTLS knobs (ca_file / require_client_cert) only make sense when
        # server-side TLS is on — without it, there's no TLS handshake at
        # which to verify a client cert. Reject the misconfiguration loudly
        # rather than silently ignoring the mTLS settings.
        if tls_cert_file is None and (tls_ca_file is not None or tls_require_client_cert):
            raise ValueError(
                "tls_ca_file / tls_require_client_cert require tls_cert_file "
                "and tls_key_file (mTLS is a layer on top of server-side TLS)"
            )
        if tls_require_client_cert and tls_ca_file is None:
            raise ValueError(
                "tls_require_client_cert=True requires tls_ca_file so the "
                "presented client cert can be verified"
            )
        self._ssl_context: ssl.SSLContext | None
        if tls_cert_file is not None and tls_key_file is not None:
            # PROTOCOL_TLS_SERVER picks the highest TLS version both ends
            # support and refuses SSLv2/3 — Python's recommended preset
            # for new server code. load_cert_chain raises FileNotFoundError
            # or ssl.SSLError on bad inputs; we let those propagate so a
            # misconfigured server fails loudly at startup rather than
            # silently falling back to plaintext.
            ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            ctx.load_cert_chain(certfile=tls_cert_file, keyfile=tls_key_file)
            # mTLS: if a CA bundle is configured, ask clients for a cert
            # during the handshake. ``CERT_REQUIRED`` rejects clients
            # without one; ``CERT_OPTIONAL`` accepts both — useful for
            # staged rollouts where some clients are not yet
            # cert-enabled.
            if tls_ca_file is not None:
                ctx.load_verify_locations(cafile=tls_ca_file)
                ctx.verify_mode = (
                    ssl.CERT_REQUIRED if tls_require_client_cert else ssl.CERT_OPTIONAL
                )
            self._ssl_context = ctx
        else:
            self._ssl_context = None
        self.tls_cert_file = tls_cert_file
        self.tls_key_file = tls_key_file
        self.tls_ca_file = tls_ca_file
        self.tls_require_client_cert = tls_require_client_cert
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
            noop_heartbeat_seconds=noop_heartbeat_seconds,
            oplog_retention_seconds=oplog_retention_seconds,
            oplog_max_entries=oplog_max_entries,
            oplog_archive_dir=oplog_archive_dir,
            cache_size=cache_size,
            session_max=session_max,
            sync_on_commit=sync_on_commit,
            durable=durable,
        )
        # Replica-set initiation: seed the bootstrap oplog noop so
        # ``local.oplog.rs`` is never empty (mongod parity). No-op when the
        # oplog is disabled or already populated.
        self.storage.ensure_oplog_bootstrap()
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
        # Per-server registry of active ``configureFailPoint`` entries.
        # Driver test suites lean on this to inject deterministic
        # errors at the wire (failCommand → errorCode / writeConcernError).
        # See ``secantus.failpoints`` for the supported subset.
        self.failpoints = FailPointRegistry()
        # Multi-document transaction state machine. The WT work is
        # bound here so the registry itself stays storage-agnostic;
        # ``txn.handle`` is None when the transaction never executed a
        # statement (the WT session is created lazily at the first one).
        self.transactions = TransactionRegistry(
            commit_func=self._commit_txn_handle,
            rollback_func=self._rollback_txn_handle,
            lifetime_seconds=transaction_lifetime_seconds,
        )

    def _commit_txn_handle(self, txn: Transaction) -> None:
        if txn.handle is not None:
            self.storage.commit_user_transaction(
                txn.handle, lsid_doc=txn.lsid_doc, txn_number=txn.txn_number
            )

    def _rollback_txn_handle(self, txn: Transaction) -> None:
        if txn.handle is not None:
            self.storage.abort_user_transaction(txn.handle)

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
        # Resolve the family from the host rather than hardcoding AF_INET, which
        # made the server IPv4-only: `host="::1"` failed at bind with a bare
        # `gaierror`, and there was no way to serve an IPv6 client at all.
        # getaddrinfo also handles hostnames, and AI_PASSIVE gives the right
        # wildcard address when the caller passes "" (INADDR_ANY / in6addr_any).
        family, socktype, proto, _canon, sockaddr = socket.getaddrinfo(
            self.host or None,
            self.port,
            type=socket.SOCK_STREAM,
            flags=socket.AI_PASSIVE,
        )[0]
        sock = socket.socket(family, socktype, proto)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.bind(sockaddr)
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
        # Drain in-flight per-connection handler threads BEFORE touching
        # WiredTiger. Each connection runs on its own daemon thread; if one is
        # mid-WT-operation when ``storage.close()`` frees the WT connection,
        # that's a use-after-free — a native crash (the intermittent xdist
        # worker death). Close every connection socket so blocked reads return
        # and the loops exit, wake any tailable getMore blocked on the oplog
        # condition variable, then wait for the active-connection count to
        # reach zero so no handler is still using storage.
        self.connections.close_all()
        self.storage.signal_shutdown()
        self._await_connections_drained(timeout=5.0)
        # Roll back open transactions while storage is still usable;
        # ``Storage.close()``'s session sweep would roll them back too,
        # but this releases their WT sessions in an orderly way first.
        self.transactions.abort_all()
        self.storage.close()

    def _await_connections_drained(self, timeout: float) -> None:
        """Block until every per-connection handler thread has exited (the
        active-connection counter reaches zero), or ``timeout`` elapses. Run
        at stop, after the connection sockets are closed and tailable waiters
        woken, so storage isn't torn down under an in-flight handler.

        ``close_all`` is re-run on every poll, not just once before this loop.
        The accept thread bumps ``_active_conns`` and spawns the handler
        *before* the handler registers its socket via ``connections.open``; a
        connection accepted in the instant before ``stop`` can therefore
        register its socket *after* the initial ``close_all`` snapshot, leaving
        its blocking ``recv`` un-woken and the drain stuck until the idle
        timeout. Re-snapshotting each poll closes such late arrivals within
        5ms (every call is idempotent — already-closed sockets are no-ops)."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            with self._active_conns_lock:
                if self._active_conns == 0:
                    return
            self.connections.close_all()
            time.sleep(0.005)
        with self._active_conns_lock:
            remaining = self._active_conns
        if remaining:
            logger.warning(
                "server stop: %d connection thread(s) still active after %.1fs; "
                "closing storage anyway. Storage's per-op `_closed` fences keep "
                "this safe (all WT access is lock-serialised against close), but a "
                "persistently stuck connection thread is a bug — its stack(s):\n%s",
                remaining,
                timeout,
                self._format_stuck_conn_stacks(),
            )

    @staticmethod
    def _format_stuck_conn_stacks() -> str:
        """Render the stacks of any still-live per-connection handler threads,
        for the stop-drain timeout warning. The thread names set in
        ``_serve_forever`` (``secantus-conn-<host>:<port>``) identify each one,
        so a shutdown wedge — historically the source of intermittent xdist
        worker deaths in this area — names its own culprit instead of surfacing
        as an opaque active-connection count."""
        frames = sys._current_frames()
        chunks: list[str] = []
        for thread in threading.enumerate():
            if not thread.name.startswith("secantus-conn-"):
                continue
            frame = frames.get(thread.ident)
            if frame is None:
                continue
            stack = "".join(traceback.format_stack(frame))
            chunks.append(f"--- {thread.name} ---\n{stack}")
        return "\n".join(chunks) if chunks else "(no connection-thread frames captured)"

    def wait(self) -> None:
        self._stop_event.wait()

    def _serve_forever(self) -> None:
        assert self._socket is not None
        while not self._stop_event.is_set():
            try:
                conn, addr = self._socket.accept()
            except OSError:
                return
            _tune_client_socket(conn)
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
            # TLS handshake (when configured) happens here, on the accept
            # thread, BEFORE _handle_client takes over. wrap_socket blocks
            # on the handshake; a malformed / plaintext client surfaces
            # as ssl.SSLError, which we log + close cleanly so a single
            # bad client can't take the daemon down or stall the loop.
            if self._ssl_context is not None:
                try:
                    conn = self._ssl_context.wrap_socket(conn, server_side=True)
                except (ssl.SSLError, OSError) as exc:
                    logger.warning("TLS handshake from %s failed: %s", addr, exc)
                    with contextlib.suppress(OSError):
                        conn.close()
                    with self._active_conns_lock:
                        self._active_conns -= 1
                    continue
            # Set an idle timeout on the socket so a hostile or stuck
            # peer can't pin a thread forever. Applies to all socket
            # reads in _handle_client.
            with contextlib.suppress(OSError):
                conn.settimeout(self.client_idle_timeout_s)
            handler = threading.Thread(
                target=self._handle_client,
                args=(conn, addr),
                # Named so a stop-drain timeout can dump exactly which
                # connection threads are still live (see
                # ``_format_stuck_conn_stacks``).
                name=f"secantus-conn-{addr[0]}:{addr[1]}",
                daemon=True,
            )
            handler.start()

    def _stream_exhaust_getmore(
        self,
        conn: socket.socket,
        response_to: int,
        reply_ids: itertools.count[int],
        body: dict[str, Any],
        ctx: CommandContext,
        first_doc: dict[str, Any],
    ) -> bool:
        """Stream the rest of an exhaust cursor over one socket.

        Called when a getMore arrives with the OP_MSG ``exhaustAllowed``
        flag set. ``first_doc`` is the reply the getMore handler already
        produced. We send that batch (and every subsequent one) with the
        ``moreToCome`` flag set, pulling further batches with synthetic
        getMores, until the cursor drains — then a final reply with
        ``id: 0`` and ``moreToCome`` clear closes the stream. mongod keeps
        the cursor alive until a getMore returns an empty batch, so even a
        cursor that drains exactly on a non-empty batch gets a trailing
        empty reply (this is what makes pymongo's command-monitoring see
        ``find, getMore, getMore, getMore`` for three docs at batchSize 1).

        Returns True if the whole stream was written (connection survives),
        False if a socket write failed (caller should drop the connection).
        """
        target_id = bson.Int64(int(body["getMore"]))
        coll = body.get("collection", "")
        ns = first_doc["cursor"].get("ns", f"{ctx.db_name}.{coll}")
        db = body.get("$db", ctx.db_name)

        def send(doc: dict[str, Any], *, more: bool) -> bool:
            flags = OP_MSG_FLAG_MORE_TO_COME if more else 0
            try:
                conn.sendall(
                    build_op_msg_reply(
                        response_to=response_to,
                        request_id=next(reply_ids),
                        body=doc,
                        flags=flags,
                    )
                )
            except OSError:
                return False
            return True

        doc = first_doc
        while True:
            cursor = doc.get("cursor")
            if not isinstance(cursor, dict):
                # An error reply (ok: 0) mid-stream — deliver it without
                # moreToCome and end the stream.
                return send(doc, more=False)
            batch = cursor.get("nextBatch", cursor.get("firstBatch", []))
            drained = int(cursor.get("id", 0)) == 0
            if drained:
                if batch and not send(
                    {
                        "cursor": {"nextBatch": batch, "id": target_id, "ns": ns},
                        "ok": 1.0,
                    },
                    more=True,
                ):
                    return False
                return send(
                    {
                        "cursor": {"nextBatch": [], "id": bson.Int64(0), "ns": ns},
                        "ok": 1.0,
                    },
                    more=False,
                )
            if not batch:
                # A live cursor that yielded nothing this round — a
                # tailable / awaitData getMore whose wait expired. Don't
                # keep streaming (that would spin forever); deliver this
                # empty batch without moreToCome and let the client fall
                # back to ordinary getMores. Normal cursors never reach
                # here (an empty batch always drains the cursor to id 0).
                return send(doc, more=False)
            if not send(doc, more=True):
                return False
            getmore: dict[str, Any] = {"getMore": target_id, "collection": coll}
            if "batchSize" in body:
                getmore["batchSize"] = body["batchSize"]
            if "maxTimeMS" in body:
                getmore["maxTimeMS"] = body["maxTimeMS"]
            getmore["$db"] = db
            try:
                doc = dispatch(getmore, ctx)
            except Exception:
                # We've already sent a `moreToCome` reply this round, so the
                # client is waiting for the rest of the stream. If the next
                # getMore blows up unexpectedly, terminate the stream with a
                # final `moreToCome`-clear reply rather than letting the
                # exception drop the connection mid-stream (which the client
                # surfaces as "Server ended moreToCome unexpectedly").
                logger.exception("error streaming exhaust getMore on cursor %d", int(target_id))
                return send(
                    {
                        "cursor": {"nextBatch": [], "id": bson.Int64(0), "ns": ns},
                        "ok": 1.0,
                    },
                    more=False,
                )

    def _stream_awaitable_hello(
        self,
        conn: socket.socket,
        response_to: int,
        reply_ids: itertools.count[int],
        body: dict[str, Any],
        ctx: CommandContext,
        first_doc: dict[str, Any],
    ) -> bool:
        """Stream awaitable (streaming-SDAM) ``hello`` replies over an exhaust
        monitor connection.

        Once a driver sees ``topologyVersion`` in a hello reply it switches its
        monitor to the streaming protocol: it sends ``hello`` with
        ``maxAwaitTimeMS`` and the OP_MSG ``exhaustAllowed`` flag, then keeps the
        connection open expecting a *continuous* stream of ``moreToCome`` replies
        (mongod holds each one until the topology changes or ``maxAwaitTimeMS``
        elapses). If the server instead answers with a single ``moreToCome``-clear
        reply, the driver's streaming monitor still treats the connection as a
        live stream — and when the socket later closes (e.g. server teardown) it
        raises ``Server ended moreToCome unexpectedly`` and clears the pool,
        failing whatever operation was in flight. That was an intermittent
        ``mongosh`` smoke failure on slower CI.

        Our topology is fixed, so we simply re-emit the same hello state every
        ``maxAwaitTimeMS`` with ``moreToCome`` set. The wait is interruptible by
        ``_stop_event`` so shutdown is prompt, and on shutdown we send one final
        ``moreToCome``-clear reply: a *clean* end of stream the driver accepts
        silently instead of surfacing as an unexpected close.

        Returns True if the stream ended cleanly (the caller drops the
        connection), False if a socket write failed (caller drops it too).
        """
        raw = body.get("maxAwaitTimeMS", 10000)
        try:
            max_await_s = max(0.0, float(raw) / 1000.0)
        except (TypeError, ValueError):
            max_await_s = 10.0

        def send(doc: dict[str, Any], *, more: bool) -> bool:
            flags = OP_MSG_FLAG_MORE_TO_COME if more else 0
            try:
                conn.sendall(
                    build_op_msg_reply(
                        response_to=response_to,
                        request_id=next(reply_ids),
                        body=doc,
                        flags=flags,
                    )
                )
            except OSError:
                return False
            return True

        # Establish the stream with the reply the handler already produced.
        if not send(first_doc, more=True):
            return False
        # Readiness check used to wake early when the socket becomes readable —
        # the client closed it or ``killOp`` shut it down, both of which surface
        # as readability/EOF. Use ``poll()`` where available (Linux / macOS):
        # ``select()`` has a hard ``FD_SETSIZE`` (1024) limit and *raises* on a
        # higher-numbered fd, so under heavy parallel load (many open sockets) a
        # monitor connection whose fd exceeded 1024 would be wrongly treated as
        # closed and dropped after the first streamed frame. ``poll()`` has no
        # such limit. Windows has no ``poll()`` but its ``select()`` isn't
        # fd-value-bounded, so it falls back safely.
        poller = select.poll() if hasattr(select, "poll") else None
        if poller is not None:
            poller.register(conn, select.POLLIN)

        def _readable(timeout_s: float) -> bool:
            if poller is not None:
                return bool(poller.poll(timeout_s * 1000.0))
            readable, _, _ = select.select([conn], [], [], timeout_s)
            return bool(readable)

        while True:
            # Hold up to maxAwaitTimeMS (topology never changes), but wake early
            # on a readable socket (client close / killOp ⇒ EOF) or on shutdown.
            # Poll in short slices so kill and shutdown are both promptly noticed
            # (a plain ``_stop_event.wait`` would leave a killed monitor
            # connection pinned until maxAwaitTimeMS elapsed).
            remaining = max_await_s
            socket_closed = False
            while remaining > 0 and not self._stop_event.is_set():
                slice_t = min(remaining, 0.25)
                try:
                    if _readable(slice_t):
                        socket_closed = True
                        break
                except OSError:
                    socket_closed = True  # socket already closed under us
                    break
                remaining -= slice_t
            if socket_closed:
                # killOp / client close: the connection is gone — drop it so the
                # handler thread is reaped and the registry entry cleared.
                return False
            if self._stop_event.is_set():
                # Clean shutdown: end the stream so the driver doesn't see an
                # unexpected mid-stream close.
                send(first_doc, more=False)
                return True
            try:
                doc = dispatch(body, ctx)
            except Exception:
                # Refresh failed — terminate the stream cleanly rather than
                # dropping the socket mid-``moreToCome``.
                logger.exception("error refreshing awaitable hello on conn %d", ctx.connection_id)
                send(first_doc, more=False)
                return True
            if not send(doc, more=True):
                return False

    def _handle_client(self, conn: socket.socket, addr: tuple[str, int]) -> None:
        # Register the socket alongside the conn_id so killOp can
        # shut it down from another thread.
        connection_id = self.connections.open((addr[0], addr[1]), sock=conn)
        reply_ids = itertools.count(1)
        connection_auth = ConnectionAuth()
        # Capture the verified client cert's subject DN once per
        # connection — MONGODB-X509 needs it to look up the user.
        # ``getpeercert()`` returns ``{}`` when the peer didn't present
        # a cert (CERT_OPTIONAL mode with a plain client), or the
        # parsed cert dict otherwise. ``getpeercert`` raises on
        # non-SSL sockets; plain TCP connections bypass this branch.
        peer_cert_dn: str | None = None
        if isinstance(conn, ssl.SSLSocket):
            with contextlib.suppress(ssl.SSLError, OSError, ValueError):
                cert = conn.getpeercert()
                peer_cert_dn = subject_dn_from_peercert(cert)
        self.metrics.connection_opened()
        logger.debug("client %d connected from %s", connection_id, addr)
        self.logs.append(
            "I", "NETWORK", "connection accepted", {"conn_id": connection_id, "from": addr}
        )
        try:
            with conn:
                while not self._stop_event.is_set():
                    # Never go idle holding a read snapshot: an idle
                    # connection whose last op left its thread session with a
                    # positioned cursor pins WT's oldest-transaction horizon
                    # for every other connection. Release before blocking.
                    self.storage.release_thread_snapshot()
                    try:
                        message = read_message(conn)
                    except ConnectionClosed:
                        return
                    except ConnectionResetError:
                        # Abrupt hang-up (RST instead of FIN) — the Go
                        # driver's tools (mongodump, mongostat, ...) close
                        # pooled connections this way routinely. A normal
                        # disconnect, not an error worth a traceback.
                        logger.debug("client %d reset connection", connection_id)
                        return
                    except TimeoutError:
                        # Idle timeout fired — drop the connection so the
                        # thread can be reaped instead of pinned forever.
                        logger.debug("client %d idle timeout, closing", connection_id)
                        return
                    except OSError as exc:
                        # The socket was closed / aborted under us — usually
                        # ``stop()`` closing this connection to drain it (on
                        # Windows that surfaces as ConnectionAbortedError /
                        # BrokenPipeError, WinError 10053/10058), or the peer
                        # hanging up abruptly. A clean disconnect, not a fault:
                        # exit the loop so the handler thread is reaped.
                        logger.debug("client %d connection closed: %s", connection_id, exc)
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
                    except (RuntimeError, OSError):
                        # OSError: stop() closed the listen socket between
                        # the None check and getsockname() — shutdown race,
                        # treat the same as "not started".
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
                            failpoints=self.failpoints,
                            transactions=self.transactions,
                            peer_cert_dn=peer_cert_dn,
                        )
                        try:
                            response_doc = dispatch(body, ctx)
                        except CloseConnectionRequested:
                            # ``failCommand`` failpoint with
                            # ``closeConnection: true``: drop the TCP
                            # connection without replying. The driver
                            # sees a closed socket and surfaces it as
                            # a client-side network error.
                            logger.debug(
                                "closeConnection failpoint fired on conn %d",
                                connection_id,
                            )
                            return
                        # `moreToCome` (bit 1) is the wire signal for
                        # fire-and-forget requests — `writeConcern: {w: 0}`
                        # unacknowledged writes use it. The spec requires the
                        # server NOT to send a reply; if we do, the driver
                        # mis-pairs it with the next response and aborts the
                        # connection with a responseTo/requestId mismatch.
                        if op.flags & OP_MSG_FLAG_MORE_TO_COME:
                            continue
                        # OP_MSG exhaust: when the client sets the
                        # `exhaustAllowed` flag on a getMore, the server
                        # streams every remaining batch back over the same
                        # socket using the `moreToCome` flag, instead of
                        # waiting for a getMore per batch. mongod only
                        # streams on getMore (the find/aggregate reply that
                        # opens the cursor is sent normally), so we mirror
                        # that — find replies fall through to the single
                        # reply below.
                        if (
                            op.flags & OP_MSG_FLAG_EXHAUST_ALLOWED
                            and "getMore" in body
                            and isinstance(response_doc.get("cursor"), dict)
                        ):
                            if self._stream_exhaust_getmore(
                                conn,
                                message.header.request_id,
                                reply_ids,
                                body,
                                ctx,
                                response_doc,
                            ):
                                continue
                            return
                        # Streaming-SDAM monitor: an awaitable `hello`/`isMaster`
                        # (carries `maxAwaitTimeMS`) sent with `exhaustAllowed`
                        # wants a continuous `moreToCome` hello stream. Honour it
                        # so the driver's monitor doesn't later raise "Server
                        # ended moreToCome unexpectedly" on teardown.
                        cmd0 = next(iter(body), "")
                        if (
                            op.flags & OP_MSG_FLAG_EXHAUST_ALLOWED
                            and cmd0 in ("hello", "isMaster", "ismaster")
                            and "maxAwaitTimeMS" in body
                            and response_doc.get("ok")
                        ):
                            if self._stream_awaitable_hello(
                                conn,
                                message.header.request_id,
                                reply_ids,
                                body,
                                ctx,
                                response_doc,
                            ):
                                continue
                            return
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
                            failpoints=self.failpoints,
                            transactions=self.transactions,
                            peer_cert_dn=peer_cert_dn,
                        )
                        try:
                            response_doc = dispatch(op.query, ctx)
                        except CloseConnectionRequested:
                            logger.debug(
                                "closeConnection failpoint fired on conn %d (OP_QUERY)",
                                connection_id,
                            )
                            return
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
            # Release the WT session cached on this connection thread so
            # the engine's session pool (default 1024) isn't leaked when
            # client churn opens many short-lived connections. Without
            # this, an aggressive driver pool (mongo-rust-driver's spec
            # runners are the canonical case) saturates the pool after
            # ~1k connections and every subsequent ``hello`` errors with
            # ``WT_ERROR: out of sessions`` mid-handshake.
            with contextlib.suppress(Exception):
                self.storage._reset_thread_session()
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
