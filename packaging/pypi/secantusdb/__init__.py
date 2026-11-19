"""SecantusDB: single-node MongoDB- and PostgreSQL-wire servers over WiredTiger.

Start a MongoDB server in two lines. It runs inside this Python process, as
SQLite does, and serves a real loopback TCP port that any driver connects to::

    from secantusdb import MongoServer

    with MongoServer() as server:
        client = pymongo.MongoClient(server.uri)

``MongoServer(embedded=False)`` runs the same server as a child process
instead, so a crash in the server cannot take this process with it.

The PostgreSQL server is beta, and runs as a child process::

    from secantusdb import PostgresServer

    with PostgresServer() as server:
        conn = psycopg.connect(server.dsn)

Both binaries are on PATH as ``secantusd-mdb`` and ``secantusd-pg``.
"""

from secantusdb._server import (
    ForkedServerError,
    MongoServer,
    PostgresServer,
    ServerStartError,
    binary_path,
)

__all__ = [
    "ForkedServerError",
    "MongoServer",
    "PostgresServer",
    "ServerStartError",
    "binary_path",
]
