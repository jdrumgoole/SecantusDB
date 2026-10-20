### Rust MongoDB server: `validateDBMetadata`, and `apiStrict` stage rules

#### Added

- `validateDBMetadata` reports what in the catalog API Version 1 does not
  allow, as mongod 8.2.11 does. It used to be 59 CommandNotFound. Under
  `apiParameters: {version: "1", strict: true}`:
  - a sparse index is listed as 323;
  - a view whose pipeline uses a stage outside the API fails the command
    with that stage's 323, nested pipelines included;
  - other indexes, validators and collection kinds pass.

  Without `db` it reads every database, in name order.

#### Fixed

- Under `apiStrict: true`, `$fill` and `$indexStats` are refused with 323,
  as on mongod; this server ran both. A stage outside API Version 1 inside
  `$facet`, `$lookup` or `$unionWith` is refused the same way. It was
  either run, or refused with a generic 2.

RM-219 part (`tests/backlog/test_rm_219_validate_db_metadata.py`).
