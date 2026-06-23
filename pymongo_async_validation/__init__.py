"""pymongo *async*-driver conformance gauge.

Runs pymongo's vendored ``test/asynchronous/`` suite — the native
``AsyncMongoClient`` code path (the successor to Motor) — against an
embedded SecantusDB. Shares the embedded-server bootstrap with the sync
gauge (``pymongo_validation.plugin``); only the in-scope test paths and
the report differ. See ``pymongo_async_validation.include_paths``.
"""
