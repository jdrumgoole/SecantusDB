### Aggregation stage specs: mongod's validation ORDER

`tools/probes/aggregation_stage_specs.py` is now **0 of 725** on the Python server, down from 22 (and 167 when the probe was written).

#### Fixed

mongod parses a stage spec field by field, so an **unknown or specifically-missing field is reported before** the generic "requires X and Y" — `{$bucket: {a: 1}}` names `a` rather than listing what is absent. Each stage has its own code and wording: `$sample` 28748, `$bucket` 40197 *with* a trailing period, `$bucketAuto` 40245 *without* one, and the IDL's 40415 for `$replaceRoot` / `$densify` / `$fill` / `$unionWith`.

Three stages invert the rule:

- **`$lookup`** reports a missing `from` first (`must specify 'pipeline' when 'from' is empty`), and only checks unknown fields once `from` is present.
- **`$graphLookup`** does the same, and its message echoes the whole spec in mongod's *spaced* document rendering — `{ a: 1 }`, not the value renderer's `{a: 1}`.
- **`$geoNear`** reports a missing `near` before objecting to the spec's own type. An array is a document in BSON, so `{$geoNear: []}` gets the `near` message while a scalar gets the type error.

Two were not message differences at all:

- **`{$unionWith: ""}` returned the outer documents unchanged** — a wrong answer — where mongod rejects the empty namespace with 73.
- **`{$documents: {}}`** is rejected while the stage is desugared into a projection, so it answers 51270 even against a collection, where every other argument gets the namespace error.

#### Changed

- The probe grew a `PROBE_SERVER` column so it measures the **Rust** server too. It had only ever compared the Python one, which is why the Rust server was **219 of 725** divergent on the same corpus with nobody the wiser. That is now **0** as well.

#### Fixed — Rust server

Three of them were wrong answers: `$out` and `$merge` with an empty target namespace returned `ok` and wrote to a nameless collection — a `$out` that silently did nothing — and `$unionWith: ""` and `$unset: ""` likewise reported success having done nothing.

The rest were three families:

- **A third value renderer.** Both `render_stage_value` and `render_value_compact` ended in `other => format!("{other:?}")`, so every type without an explicit arm reached the client as Rust `Debug` — `Regex { pattern: "a", options: "" }` where mongod says `/a/`. And the two are not interchangeable: the 40228 / 17053 family quotes binary (`BinData(0, "7A")`) and wraps code as `Code("x=1")` where `$limit`'s renders `BinData(0, 7A)` and the code text bare.
- **`map_err(|_| Fallback::Defer)`** on seven stage dispatches discarded the real error — the same shape as the storage layer's `map_err(|_| QueryUnsupported)`. On the standalone server that reads as "the *stage* is unsupported" for what is a bad *argument* to a supported one.
- **Validation order**, the same rule as above, ported stage by stage.

`$set` also reported its errors as `$addFields`, because the two share an implementation and the message was hard-coded to the alias.
