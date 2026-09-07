### The Rust server errored on case-insensitive queries, and had no collated order

`tools/probes/collation_order.py` was the one Rust-aware probe never swept
against the Rust server. Sweeping it found two things, the first of them a hard
failure on ordinary input.

**A case-insensitive query or sort over any non-ASCII text errored.**

```
find().sort("v", 1).collation({locale: "en", strength: 1})   over ["a","A","á","B","b"]
    mongod / python  ['a', 'A', 'á', 'B', 'b']
    rust             2 BadValue: an indexed value is of a type the
                     Rust server does not support
```

Not only accents — `ß` and `日` triggered it too, on match filters as well as
sorts. `normalize_index_bytes` had `if !s.is_ascii() { return None }`, meaning
"defer to the pure engine"; that is right on the Python server and wrong on the
Rust one, which has no Python behind a defer. It now folds properly: NFKD, drop
the combining marks, full case fold.

There were **two** such bails, not one. Fixing the index encoder left the query
path — `normalize`, behind `collation::equal` / `compare` — still deferring, so
collated equality and range queries on non-ASCII text kept answering `2 BadValue:
query uses a construct the Rust server does not support` after the sort was
fixed. Both now share one fold helper so they cannot drift apart again.

The fold has to be *case folding*, not `to_lowercase`: folding maps `ß` to `ss`,
and mongod sorts `["ß","s","t"]` as `["s","ß","t"]` — which only comes out right
if `ß` compares as `ss`.

**Collated ordering was not implemented at all** — strings sorted by codepoint,
so every accented word landed after `z`. `collation.py`'s `sort_levels` is now
ported to `collation::sort_level_bytes` as a byte-comparable three-level key:
the measured `MARK_ORDER` table (acute before grave, which is *not* codepoint
order), the `backwards` reversal that makes `cote < côte < coté`, the
`caseFirst` flip, and `numericOrdering` digit runs so `a2 < a10`.

The probe goes from dying at case 10 to **0 unexpected divergences of 19** — the
2 remaining are the documented Swedish / Danish locale gaps that need CLDR data.
A wider three-way sweep across every collation option shape is **0 of 64**
against mongod.

**Nothing on disk changes**, and the encoders are now split so that stays true
by construction. `sortkey::encode_value` remains the INDEX encoder — single-level
fold, byte-identical to Python's `encode_value(collation=)`, which is what the
entries table holds and what the other server reads back. The three-level
ordering key lives in a new `encode_sort_value`, used only by the in-memory
sort-key builder. Collapsing both roles into `encode_value` is what broke
`test_collation_encoding_parity`: the same function name means index bytes in
Python and had come to mean the sort key in Rust. The probe's own invariant
confirms the result — index and non-index results agree on every case, so an
index still changes speed and never results.

**Mark filtering uses the predicate Python uses, per site.** These are three
different sets and the difference is measurable: `_strip_accents` filters general
category `Mn` alone, while `sort_levels` filters on a nonzero canonical combining
class. `unicode_normalization::char::is_combining_mark` is neither — it is true
for all of `M*`, so using it for the accent strip dropped a Devanagari vowel sign
(U+093E, category `Mc`) that the Python engine keeps. Measured 2026-09-07.

Adds direct `unicode-normalization` and `unicode-properties` dependencies to
`secantus-core`; both were already in the lock tree transitively.

Filed, not fixed: the **Python** server orders ligatures wrongly under a
collation (`["ﬁ","fi","fj"]`), which this port surfaced. `sort_levels`
decomposes with NFD, which does not split a compatibility ligature, so the
secondary level compares a one-group key against a two-group one. The obvious
fixes are unmeasured — see `tasks/backlog.md`.

#### Fixed

- `secantus-core`: `normalize_index_bytes` and the query-path `normalize` both
  fold non-ASCII instead of deferring; new `sort_level_bytes` three-level
  ordering key behind a new `encode_sort_value`, keeping the on-disk
  `encode_value` untouched; accent stripping filters category `Mn` (not all of
  `M*`); `Collation` carries `caseFirst` and `backwards`.
