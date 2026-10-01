### Python PostgreSQL server: full-text search follows PostgreSQL

The Python PG server's text search was a simplified stand-in. It is now a port of PostgreSQL's own (tsvector / tsquery I/O, the document parser, ranking, headlines, phrase search). Corpora `fts`, `fts2` and `fts_langs` went from 106 divergences to 0 against PostgreSQL, and no corpus got worse.

#### Added

- A new `fts` extra: `pip install "SecantusDB[fts]"` installs `snowballstemmer==2.2.0`, the Snowball release PostgreSQL 15 ships. With it, the 26 non-English text search configurations stem and drop stop words exactly as PostgreSQL does. Without it they refuse with `0A000` rather than answering differently.
- `setweight` (two and three arguments), `ts_delete`, `ts_filter`, `tsquery_phrase` and `get_current_ts_config` are implemented.

#### Fixed

- tsvector and tsquery input and output follow PostgreSQL's rules: ordering, positions, weights, quoting, and the exact error codes and messages. `'x'::tsquery` is no longer stemmed.
- The document parser recognises emails, hosts, URLs, numbers, versions and hyphenated words. The English stop-word list is complete.
- `ts_rank` / `ts_rank_cd` match PostgreSQL's arithmetic and return `real`.
- Operators: `!!tsquery`, `tsquery <-> tsquery`, `tsvector @@ 'literal'` and `text @@ text` now work as PostgreSQL defines them.
- An unknown text search configuration answers `42704`.
