"""Capability declarations for the SQLAlchemy compliance gauge.

``SuiteRequirements`` is SQLAlchemy's designed extension point for exactly
this: each ``@property`` names a capability, and the compliance suite skips
the tests that need anything not declared open. The stock defaults describe a
conservative baseline dialect; this subclass is where SecantusDB's SQL server
opens more of them as they are verified.

Keep this honest: only open a capability after the corresponding suite tests
pass against the server — an over-claim converts skips into failures, which
is the right direction to fail in, but do it deliberately.
"""

from __future__ import annotations

from sqlalchemy.testing import exclusions
from sqlalchemy.testing.requirements import SuiteRequirements


class Requirements(SuiteRequirements):
    @property
    def schemas(self):
        # Tables are namespaced per schema (stored under dotted catalog keys,
        # like user types), so ``test_schema.users`` and ``public.users``
        # coexist and reflect under their own pg_namespace rows.
        return exclusions.open()

    @property
    def views(self):
        return exclusions.open()

    @property
    def temp_table_names(self):
        # Temp tables are session-scoped (created flagged temp, visible only
        # to their creating session, dropped at connection teardown), so
        # listing them works and the suite's expectations line up.
        return exclusions.open()

    @property
    def has_temp_table(self):
        return exclusions.open()

    @property
    def reflects_pk_names(self):
        return exclusions.open()

    @property
    def unique_constraint_reflection(self):
        return exclusions.open()

    @property
    def unique_constraints_reflect_as_index(self):
        # A UNIQUE constraint's backing index shows in get_indexes with
        # duplicates_constraint set, matching real PG.
        return exclusions.open()

    @property
    def foreign_key_constraint_reflection(self):
        return exclusions.open()

    @property
    def datetime_microseconds(self):
        # BSON datetimes are int64 milliseconds — the shared document store
        # (the dual-protocol view is the product) can't hold sub-millisecond
        # precision without a storage-representation change. The same switch
        # MySQL-family dialects close. tasks/backlog.md tracks the divergence.
        return exclusions.closed()

    @property
    def supports_distinct_on(self):
        # DISTINCT ON is implemented (evaluated path keeps the first row per
        # key in ORDER BY order).
        return exclusions.open()

    @property
    def index_reflects_included_columns(self):
        # get_indexes returns include_columns (the postgres dialect always
        # emits it; INCLUDE columns themselves reflect as empty lists).
        return exclusions.open()
