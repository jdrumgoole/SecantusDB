### Rust PostgreSQL server: all 907 of PostgreSQL 15.19's ICU collations

`pg_collation` listed 897 of PostgreSQL 15.19's 907 ICU collations; the
other ten were refused (`42704`): `mr`, `mr-IN`, `kok`, `kok-Deva`,
`kok-Deva-IN`, `as`, `as-IN` — whose conjunct rule (`&ह<क्ष`, `&হ<ক্ষ`)
ICU4X's collator does not apply, with its compiled data or with data
generated from ICU's release — and `kok-Latn`, `kok-Latn-IN` and
`en-US-u-va-posix`, left out with them.

All ten are served now (RP-005). The conjunct tailoring is applied on top of
ICU4X's collator by a literal rewrite before sort-keying: `क्ष` sorts
immediately after `ह`, then `ज्ञ`; Assamese `ক্ষ` after `হ` — measured on
PostgreSQL 15.19, and stable for conjuncts carrying vowel marks (`क्षा`
after `हा`, via a symbol-range marker whose primary sits above every script
letter). `kok-Latn` orders as the root collation, as on PostgreSQL.
`en-US-u-va-posix` is its variant's codepoint order — ` `<`!`<`0`<`9`<`Z`<
`a`, `a0`<`aa`, `~` last, measured on PostgreSQL 15.19 — which is exactly
what the byte-order comparison produces, so the collation resolves to it.
Unit tests in `collation.rs` pin each rule.

#### Fixed

- `COLLATE "mr-x-icu"` (and the nine siblings) are accepted and order as
  PostgreSQL 15.19, closing the last gap in the ICU collation list
  (fixes RP-005,
  `tests/backlog/test_rp_triage.py::test_marathi_icu_collation_orders_as_postgres`,
  fixed-marked).

#### Added

- `pg_icu_collations.tsv`: the ten previously omitted PostgreSQL 15.19
  rows (oids, ICU locales, collversions read from a live reference).
- `collation.rs`: the `Built::Rewritten` collator wrapper and per-locale
  conjunct rules; the `kok-Latn`→root and `va-posix`→byte-order mappings.
