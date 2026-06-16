"""Driver-conformance gauge for the low-level PHP extension (mongo-php-driver).

Runs the unmodified ``.phpt`` suite from ``vendor/mongo-php-driver/tests/``
against a standalone SecantusDB daemon, using PHP's ``run-tests.php`` harness
and the already-installed ``mongodb`` extension. See ``runner.py``.
"""
