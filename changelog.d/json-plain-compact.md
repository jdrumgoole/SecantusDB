### Plain json columns render compact, jsonb keeps its canonical spacing

PostgreSQL treats `json` and `jsonb` output differently: a `json` value's
text is preserved verbatim from input, while `jsonb` re-renders in its
canonical spaced form (`{"a": 1, "b": 2}`). SecantusDB rendered both from
the parsed stored value with jsonb's spacing, so a client that inserted
compact JSON into a `json` column — which is what every machine-serialized
payload looks like — got visibly different bytes back from `SELECT` and
`COPY TO`.

A plain `json` (oid 114) column now renders compact (`{"abc":"def"}`)
across the simple protocol, the extended protocol (text and binary
formats), and both COPY TO forms, reproducing typical input byte-for-byte;
`jsonb` keeps PG's canonical spacing. Full verbatim text preservation is
deliberately out of scope: the parsed-subdocument storage shape is what
lets json-path filters push down to indexed storage lookups, so a
hand-spaced `json` literal still re-renders normalised.

#### Fixed

- `sql/typemap.py` / `sql/pgserver.py` / `sql/pgextended.py` /
  `sql/engine.py`: plain `json` (oid 114) result columns render compact in
  DataRows (text + binary), `COPY table TO STDOUT`, and
  `COPY (SELECT …) TO STDOUT`; jsonb rendering is unchanged.
