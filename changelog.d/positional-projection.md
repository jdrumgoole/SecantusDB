### Positional `$` projection (both servers)

`find()`'s positional projection operator now works on both servers:
`find({"items.k": "b"}, {"items.$": 1})` returns only the **first array element
that matched the query** on that path — `items: [{k: "b", …}]` — instead of the
whole array stripped to empty documents, which is what both servers previously
produced. The matched element is resolved from the query's clause on the array
(a dotted `items.sub` field, a direct value/range on `items`, or an
`items: {$elemMatch: …}`), so it works for arrays of documents and arrays of
scalars alike. Found by a three-way projection differential against real
`mongod` 6.0; all value cases match exactly.

#### Fixed

- `projection.py` / `secantus-core`: the positional `$` projection resolves and
  returns the first query-matched array element. The find command threads the
  filter into the projection engine so the operator has the query context it
  needs. Validation is parse-time (matching mongod), so an invalid positional —
  more than one (`Location31276`), an exclusion form (`Location31395`), or an
  array field the query doesn't reference (`Location51246`) — errors even when the
  query matches nothing. The Python server reproduces mongod's exact Location
  codes; the Rust server surfaces a generic `BadValue` on these error paths (the
  documented cross-cutting error-code gap). (`$meta` projection remains deferred —
  `tasks/backlog.md` §7.5.)
