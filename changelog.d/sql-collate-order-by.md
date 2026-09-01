### `ORDER BY … COLLATE "en_US.UTF-8"` actually collates

A `COLLATE` clause on `ORDER BY` was accepted and then ignored — the rows came
back in byte order, so a query that asked for a locale ordering silently got a
different one. An unknown collation name was accepted too, where PostgreSQL
rejects it.

The default ordering is **unchanged and was never wrong**: SecantusDB sorts
text by bytes, which is exactly what a PostgreSQL database created with the `C`
collation does. It now says so, rather than reporting an empty collation name.

Naming a locale gives the ordering you would expect — case and punctuation
stop dominating, and accented letters sort beside their base letter instead of
after `z`:

```
default / COLLATE "C"     ABC, Abc, ZZZ, a b, a-b, aBc, ab, abc, zzz
COLLATE "en_US.UTF-8"     a b, a-b, ab, abc, aBc, Abc, ABC, zzz, ZZZ
```

This needs no ICU library: it reuses the collation ordering already built for
the MongoDB side. Two differences from PostgreSQL remain and are documented in
the tests — `ß` is not expanded to `ss`, and `-` and `_` take different
relative weights.

#### Fixed

- `ORDER BY … COLLATE "<locale>"` orders by that collation instead of by bytes.
- An unknown collation reports `42704 collation "…" for encoding "UTF8" does
  not exist`, as PostgreSQL does.
- `SHOW lc_collate` and `SHOW lc_ctype` report `C` instead of an empty string.
- `pg_collation` lists the collations that can be used, instead of being empty.
