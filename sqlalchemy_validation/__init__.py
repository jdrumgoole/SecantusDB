"""SQLAlchemy dialect-compliance gauge for the SQL / PostgreSQL server.

The G6 gauge of ``tasks/sql-gauges-plan.md``: SQLAlchemy's own third-party
dialect compliance suite (``sqlalchemy.testing.suite`` — the framework's
built-in "declare what you support, run the rest unmodified" mechanism) run
against a daemon ``SecantusPGServer`` through the stock ``postgresql+psycopg``
dialect. Nothing is vendored — the suite ships inside the ``sqlalchemy``
package already pinned in the ``dev`` extra.

Capability declarations live in ``requirements.py`` (a
``SuiteRequirements`` subclass); growing the gauge means opening more
capabilities there and keeping the numbers honest.
"""
