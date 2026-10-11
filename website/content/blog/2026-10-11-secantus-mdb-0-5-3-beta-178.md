Title: secantus-mdb 0.5.3-beta.178 sorts and matches strings by collation the way mongod does
Date: 2026-10-11 09:00:00
Slug: secantus-mdb-0-5-3-beta-178
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-mdb 0.5.3-beta.178 replaces its hand-written collation with ICU4X, makes a collation reach aggregation, defaults and unique indexes, and fixes renameCollection dropping a collection's validator.

If your tests use a collation on `secantusd-rs`, this release changes their
answers, and the new ones are `mongod`'s.

```python
people.find({"name": {"$lte": "B"}}, collation={"locale": "en"})
```

Take a collection holding the names `a`, `A`, `á`, `b`, `B` and `c`. `mongod`
returns the first five, which is English order. The Rust server compared code
points, so it returned `A` and `B` and missed the rest. A case-insensitive
equality match worked. Most other things did not.

We measured it the usual way, by sending the same commands to the server and
to `mongod` 8.2.11 and comparing every reply. Of 764 collation scenarios, the
server answered 462 differently. It now differs in 8.

**What was wrong.** The server had its own approximation of collation: fold
the case, strip the accents, compare what is left. That gets equality right
for plain letters and gets order wrong, because a language's order is not the
order of its code points.

- Ranges (`$gt`, `$lte`) and sorts put every capital before every small
  letter, and punctuation in the wrong place.
- `numericOrdering` was refused outright.
- Aggregation ignored the collation. `{$eq: ["a", "A"]}` was false at
  strength 2, `$group` made two groups of `a` and `A`, and the `$sort` stage
  sorted by code point.
- A collection created with a default collation did not use it.
- `{locale: "zz_nope"}`, `{strength: 6}` and a document with no `locale` were
  all accepted.

**What it does now.** `mongod` hands collation to ICU. The server now hands
it to ICU4X, the Rust implementation of the same algorithm and locale data.
Before adopting it we sorted 157 strings under 40 collations on both: every
option, 24 locales. The order and the sets of equal strings were the same,
with one exception given below.

So `strength`, `caseLevel`, `caseFirst`, `numericOrdering`, `alternate`,
`maxVariable` and `backwards` all work, for the 109 locales `mongod` accepts.
A collation applies in expressions, group keys, the `$sort` stage, `$lookup`
and the update operators (`$pull`, `$addToSet`, `$push` with `$sort`, `$min`,
`$max`, `arrayFilters`). A collection's default collation is inherited by its
indexes and by every command that names none. A malformed collation gets
`mongod`'s error.

**Unique indexes.** A unique index with a collation compared bytes, so at
strength 2 it stored `"a"` and `"A"` side by side. It now refuses the second,
on insert, on update and when the index is built. An index you built with an
earlier version keeps the old behaviour until you drop and rebuild it.

**Three bugs found on the way** that have nothing to do with collation:

- `renameCollection` dropped the collection's options. A collection with a
  validator had none after a rename, and every later write went unchecked.
  The options and the UUID now move with the collection.
- `$group` with `$min` or `$max` kept the first of two equal values. `mongod`
  keeps the last. You can see it when the values are equal and not identical,
  such as `1` and `1.0`.
- A sort inside `$top`, `$bottom`, `$setWindowFields` or `$fill` compared an
  array as a whole. `mongod` sorts an array by its smallest element, or its
  largest when descending.

What this release does not do:

- ICU4X always treats two spellings of the same accented letter as equal.
  `mongod` does so only with `normalization: true`.
- `mongod` 8.2 uses ICU 57 and ICU4X carries newer language data, so a
  character added to Unicode since may sort differently. We found none.
- A query with a collation scans the collection. It does not use an index.
- With a default collation, `_id` values `"a"` and `"A"` can both be
  inserted. `mongod` refuses the second.
- Five collation variants are refused by name: `search`, `searchjl`,
  `big5han`, `gb2312han` and `phonetic`.

The binary is 1.5 MB larger, which is the language data. We ran the pymongo,
Go and Node driver suites against this code and each gave the same result as
the release before.

This is a server for tests. It is single-node and it is a beta.

`cargo install secantus-mdb --version 0.5.3-beta.178` builds it from crates.io.
Binaries for Linux x86_64, macOS arm64 and Windows x86_64 are on the release.

[Rust MongoDB server](https://secantusdb.com/rust-db.html) ·
[secantus-mdb on crates.io](https://crates.io/crates/secantus-mdb) ·
[MongoDB binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.178)
