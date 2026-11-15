### A NOT is pushed onto the clause it negates

PostgreSQL's planner never keeps a NOT over something it can negate: `negate_clause` applies De Morgan to an AND or an OR, turns a BETWEEN into the OR of its negated comparisons, and replaces a comparison, LIKE, regex match or IN list by its negator and a null test by its opposite. The AND that comes out joins the scan's other quals, where it can become index conditions. The Rust PostgreSQL server's EXPLAIN now plans a NOT the same way, including one whose pushed-down AND meets a NULL arm and becomes a constant-false `One-Time Filter`.

#### Fixed

- EXPLAIN pushes a WHERE's NOT onto its arms and operators, as `negate_clause` does.
  Fixes RP-412 (tests/backlog/test_rp_explain_costs.py::test_plan[a not over an or is the and of its negated arms], tests/backlog/test_rp_explain_costs.py::test_plan[a not over a between is the or of its negators], tests/backlog/test_rp_explain_costs.py::test_plan[a not over an or with a null arm is a false one-time filter])
