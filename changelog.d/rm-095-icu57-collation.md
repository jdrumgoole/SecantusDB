### The Rust MongoDB server collates with mongod's own ICU

Every collated comparison in mongod is ICU 57.1's answer, and now so is the
Rust MongoDB server's. It links the same ICU, built from mongo's own copy of
the sources and its trimmed locale data, and makes the calls mongod makes. Before,
the server used ICU4X, a Rust reimplementation with newer data. ICU4X
agreed with mongod on order but could not do three things. It could not tell
two canonically equivalent spellings apart under `normalization: false`, the
default. Its data lacked five collation types mongod accepts: `search`,
`searchjl`, `big5han`, `gb2312han` and `phonetic`. And its sort keys had
different bytes, so a duplicate-key error quoted a different key.

What a locale defaults to now comes from ICU too, as it does on mongod: an
index on `{locale: "da"}` sorts capitals first and says so in
`listIndexes`, a `fr_CA` query at strength 1 is refused because that locale
compares accents backwards, and which locale names are valid, with the
"Did you mean" each refusal suggests, is ICU's reading of the name. A store
written before this change has its collated indexes and collated `_id`
claims re-keyed the first time it opens.

#### Fixed

- `crates/secantus-icu` (new): builds mongo r8.2.11's
  `src/third_party/icu4c-57.1`, vendored at `vendor/icu4c-57.1`, from the
  source lists, defines and warning switches of its `BUILD.bazel`, and hands
  ICU the embedded `icudt57l.dat` (`icudt57b.dat` on big-endian) through
  `udata_setCommonData`, as mongod's `icu_init.cpp` does. A C++ shim makes
  mongod's `icu::Collator` calls (`createInstance`, `get/setAttribute`,
  `setMaxVariable`, `compareUTF8`, `getCollationKey`).
- `secantus-core::collation`: compares, sorts and keys through it. The spec
  is resolved as `updateCollationSpecFromICUCollator` does it: a given
  option is set, an absent one is read from the locale. `create` fills the
  IDL defaults first, as mongod does. The spec checks run on the resolved
  options. Locale validation is ICU's (`ULOC_VALID_LOCALE` against the name
  given), replacing the hand-built locale table and its three-letter-code
  reconstruction. Strings that are not UTF-8 compare through
  `compareUTF8`, with each invalid sequence weighed as U+FFFD.
- Sort keys are mongod's bytes (`"W\x01\x05"` for `"x"` at `en` strength
  2). `collation::KEY_FORMAT` is 2: collated indexes built earlier are
  rebuilt on open. The collated `_id` claim marker now records its format,
  and claims under an older one are dropped and taken again, so a second
  spelling of a stored `_id` is still refused.
- Measured against mongod 8.2.11: 2,202 locale, type, default and refusal
  cases all equal, and `tools/probes/collation.py` down from 4 divergent
  shapes to 2 (the unbuilt hashed and text indexes), now with sort-key
  bytes compared, not scrubbed. A duplicate document `_id` (RM-092) now
  quotes mongod's exact key bytes, and its gap test asserts them.
- Fixes RM-095 (tests/backlog/test_rm_085_112.py::test_five_collation_types_are_accepted)
  and RM-091 (tests/backlog/test_rm_085_112.py::test_canonically_equivalent_strings_differ_without_normalization).
  Pinned by `tests/mongod_live/test_collation_icu.py`, the `secantus-icu` and
  `collation` unit tests, and
  `secantus-storage`'s `open_rekeys_collated_id_claims_of_an_older_key_format`.
- The PostgreSQL server links `secantus-core` and so builds ICU 57.1 too.
  Its own collations still go through `secantus-pgplan`'s ICU4X.
- `tools/probes/_servers.py`: a replica-set probe now starts the embedded
  Rust server with the same replica set. It had been compared as a
  standalone against a replica-set mongod, and `time_series.py` showed
  22 false divergences, all `commitQuorum`.
- The release archives ship ICU's licence as `LICENSE-ICU`.
