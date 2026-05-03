from __future__ import annotations

import contextlib
import itertools
import logging
import socket
import threading
from types import TracebackType
from typing import Any, Self

from secantus.commands import CommandContext, dispatch
from secantus.cursors import CursorRegistry
from secantus.storage import Storage
from secantus.wire import (
    ConnectionClosed,
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


class SecantusDBServer:
    def __init__(
        self,
        host: str = "127.0.0.1",
        port: int = 0,
        storage_path: str = "./secantus-data",
    ) -> None:
        self.host = host
        self.port = port
        self._socket: socket.socket | None = None
        self._thread: threading.Thread | None = None
        self._stop_event = threading.Event()
        self._connection_ids = itertools.count(1)
        self.storage = Storage(storage_path)
        self.cursors = CursorRegistry()

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
            handler = threading.Thread(
                target=self._handle_client,
                args=(conn, addr),
                daemon=True,
            )
            handler.start()

    def _handle_client(self, conn: socket.socket, addr: tuple[str, int]) -> None:
        connection_id = next(self._connection_ids)
        reply_ids = itertools.count(1)
        logger.debug("client %d connected from %s", connection_id, addr)
        try:
            with conn:
                while not self._stop_event.is_set():
                    try:
                        message = read_message(conn)
                    except ConnectionClosed:
                        return
                    except WireProtocolError as exc:
                        logger.warning("wire protocol error from %s: %s", addr, exc)
                        return

                    op = message.op
                    if isinstance(op, OpMsg):
                        body = _merge_op_msg_body(op)
                        ctx = CommandContext(
                            connection_id=connection_id,
                            storage=self.storage,
                            cursors=self.cursors,
                            db_name=body.get("$db", "admin"),
                        )
                        response_doc = dispatch(body, ctx)
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
