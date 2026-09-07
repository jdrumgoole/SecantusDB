### The Rust server's `explain` now builds mongod's stage tree

mongod wraps the scan in the stages that describe the rest of the query. The
Rust server reported the bare scan node, so a client got:

| query | mongod | Rust before |
| --- | --- | --- |
| `limit: 3` | `LIMIT` → `COLLSCAN` | `COLLSCAN` |
| `skip: 3` | `SKIP` → `COLLSCAN` | `COLLSCAN` |
| `limit: 3, skip: 2` | `LIMIT` → `SKIP` → `COLLSCAN` | `COLLSCAN` |
| `projection: {a: 1}` | `PROJECTION_SIMPLE` → `COLLSCAN` | `COLLSCAN` |
| `sort: {zzz: 1}` | `SORT` → `COLLSCAN` | `COLLSCAN` |

The most useful consequence of getting this right: **a client asking "is my
sort served by an index?" reads the answer off the presence of a blocking
`SORT`**, which is the question `explain` is usually run to answer. Without the
tree there was nothing to read.

The nesting is mongod's own and is not the order the command's fields are
written in — a blocking `SORT` sits directly above the scan and ABSORBS the
limit (as `limitAmount`, counting the documents the skip will later discard, so
no separate `LIMIT` appears); `SKIP` sits above that; the projection above the
skip; an unabsorbed `LIMIT` outermost.

Deciding whether a sort needs a blocking stage takes a `sorted_by_index` flag,
which `ExplainPlan::IxScan` did not carry — the walk comes out in sort order
only when the index's LEADING field is the one being sorted on. It is now
plumbed from `make_ixscan_plan` and both hint branches through the storage
adapter.

**The Rust server now diverges from mongod on exactly the same seven shapes as
the Python server**, which are that server's documented floor: four are
`indexBounds` (which this project deliberately does not reproduce, along with
`rejectedPlans` and the IDHACK / EXPRESS_IXSCAN / COUNT_SCAN / DISTINCT_SCAN
executors) and three are a genuine cost-model difference where mongod picks an
IXSCAN and we pick COLLSCAN + SORT, returning identical documents. `explain` is
at parity between the two servers.

#### Fixed

- `secantus-core`: `build_stage_tree` and `projection_stage_name`, ported from
  `secantus.explain`.
- `secantus-storage`: `ExplainPlan::IxScan` carries `sorted_by_index`, set at
  every construction site; the storage adapter surfaces it.
- `secantus-commands`: `explain` wraps the scan node in the stage tree for
  `find` (`count` / `distinct` keep their flat node — their `COUNT_SCAN` /
  `DISTINCT_SCAN` vocabulary has not been measured).
