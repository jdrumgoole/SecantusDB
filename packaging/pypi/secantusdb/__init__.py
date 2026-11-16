"""SecantusDB: single-node MongoDB- and PostgreSQL-wire servers over WiredTiger.

This package carries the two Rust servers as prebuilt binaries and a small
launcher, so a test can start a real server in two lines::

    from secantusdb import MongoServer

    with MongoServer() as server:
        client = pymongo.MongoClient(server.uri)

The PostgreSQL server is beta::

    from secantusdb import PostgresServer

    with PostgresServer() as server:
        conn = psycopg.connect(server.dsn)

The same binaries are on PATH as ``secantusd-mdb`` and ``secantusd-pg``.
"""

from secantusdb._server import (
    MongoServer,
    PostgresServer,
    ServerStartError,
    binary_path,
)

__all__ = ["MongoServer", "PostgresServer", "ServerStartError", "binary_path"]
