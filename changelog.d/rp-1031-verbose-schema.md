### A structured EXPLAIN VERBOSE names each scan's schema

Under VERBOSE, PostgreSQL's JSON, YAML and XML plans name the schema of
every scanned relation beside its name, `"Relation Name": "e", "Schema":
"public"`. SecantusDB's PostgreSQL server left the `Schema` key out; it now
prints it, in PostgreSQL's place between the relation's name and its alias.

#### Fixed

- Rust PG server: a structured EXPLAIN VERBOSE prints each scan's `Schema`
  (`ExplainScanTarget`).
  Fixes RP-1031 (tests/backlog/test_rp_explain_costs.py::test_plan[a structured verbose scan names its schema])
