### Rust PostgreSQL server: the ispell and thesaurus dictionaries

`CREATE TEXT SEARCH DICTIONARY` takes the `ispell` and `thesaurus`
templates, as PostgreSQL 15.19 does. Every statement of PostgreSQL's own
`tsdicts` regression test now answers as 15.19 does: 131 of 131, where 4
differed once the templates were in.

#### Added

- **The `ispell` template**, a transcription of PostgreSQL's `spell.c` and
  `regis.c`. It reads:
  - a dictionary file, and an affix file in ispell's own format or in
    MySpell/Hunspell's (`PFX` / `SFX`, `FLAG long` / `num`, `AF` aliases,
    the `COMPOUND*` options);
  - an optional `StopWords` list.

  It gives a word's normal forms. When the affix file allows compounds, it
  also gives the parts a compound word splits into, each split its own
  variant (`footballklubber` is `footballklubber`, `foot & ball & klubber`
  or `football & klubber`).
- **The `thesaurus` template**, a transcription of `dict_thesaurus.c`.
  - A thesaurus file's phrases are normalised through a subdictionary,
    which can be built-in or a user one.
  - A phrase is replaced by its substitute, and a `*word` is kept as
    written.
  - `ts_lexize` asks a second time with `isend`, as PostgreSQL's does.
- The dictionary files are the samples PostgreSQL installs in
  `tsearch_data`. They are carried in the binary beside the stop-word and
  synonym files, so any other name is a missing file.
- **A multi-word dictionary in a configuration.** `to_tsvector`, the
  tsquery builders and `ts_headline` read a document through a port of
  `ts_parse.c`'s `LexizeExec`. While a thesaurus phrase may still match,
  the thesaurus is handed the following tokens. When it fails, the tokens
  go back to one-at-a-time.
- **Several lexemes at one position.** A dictionary's variants become a
  tsquery's OR, and a variant's lexemes its AND (`pushval_morph`). A
  substitute's later words take the following positions (`TSL_ADDPOS`).
- `ts_debug` names the first dictionary whose `ts_lexize` recognises a
  token, and an error a dictionary raises now reaches the client. It was
  dropped before.

#### Fixed

- `dictinitoption` writes an option given as a number unquoted
  (`casesensitive = 1`), as `serialize_deflist` does. It had quoted every
  value.
  Fixes RP-250 (tests/backlog/test_rp_batch73.py::test_scenario[dictinitoption writes a number unquoted]).
- `ALTER TEXT SEARCH CONFIGURATION ... ADD / DROP MAPPING FOR word, word`
  takes a token type named twice once, as 15.19's `getTokenTypes` does.
  The `ADD` had failed with 23505, and the `DROP` with 42704.
  Fixes RP-251 (tests/backlog/test_rp_batch73.py::test_scenario[a token type named twice in ALTER MAPPING]).
- A synonym written `index*` matches as a prefix in a tsquery
  (`'index':*`). The flag `dict_synonym.c` sets was never carried, so the
  query lost its `:*`. That old answer is read from the code: no build
  without the fix was measured.
  Fixes RP-252 (tests/backlog/test_rp_batch73.py::test_scenario[a synonym's prefix output in a tsquery]).
- The `ispell` and `thesaurus` templates are no longer refused with 0A000.
  This closes RP-169.
  Fixes RP-169 (tests/backlog/test_rp_batch51.py::test_scenario[ispell and thesaurus dictionaries],
  tests/backlog/test_rp_batch73.py::test_scenario[ispell dictionaries in ts_lexize],
  tests/backlog/test_rp_batch73.py::test_scenario[unsuitable ispell affix and dictionary files],
  tests/backlog/test_rp_batch73.py::test_scenario[ispell and thesaurus parameters],
  tests/backlog/test_rp_batch73.py::test_scenario[a thesaurus in ts_lexize],
  tests/backlog/test_rp_batch73.py::test_scenario[ispell dictionaries in a configuration],
  tests/backlog/test_rp_batch73.py::test_scenario[a thesaurus in a configuration]).

#### Changed

- PostgreSQL's quicksort (`pg_qsort`) moves from `secantus-pg` to
  `secantus-pgplan`, so that the dictionaries sort as PostgreSQL's do.
