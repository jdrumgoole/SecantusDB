### Bind validates its parameter count

A Bind message must supply exactly as many parameters as the prepared
statement has, and the parameter types the client declared at Parse count
even when the query text uses fewer placeholders — declaring three and
binding one now raises PostgreSQL's 08P01 instead of silently executing with
whatever arrived. COPY keeps its own 08P01 (with PG's statement-summary
detail) for the same mistake. The pgtest `prepare` corpus file pins both and
is now green.

#### Fixed
- Bind accepted a parameter count that disagreed with the prepared
  statement's.
