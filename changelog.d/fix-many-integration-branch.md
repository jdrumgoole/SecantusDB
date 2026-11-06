### Fix-one batches land on an integration branch; pg-oracle runs in three shards

`scripts/fix_many.sh -i <branch>` makes each fix-one session gate locally and push its fix to an integration branch, with no PR and no CI wait. The batch then opens one PR from that branch to `main`, and CI runs once for all of its fixes. Each fix stays its own commit, so a red run can be bisected. The slowest CI check, `pg-oracle (server)`, took 35–40 minutes. Its backlog regression and gap tests now run in a third shard, `pg-oracle (backlog)`.

#### Changed

- `scripts/fix_many.sh`: a new `-i BRANCH` option.
- `scripts/fix_one.sh` and the fix-one skill: a new `into <branch>` argument.
- `.github/workflows/test.yml`: `pg-oracle` runs as three shards, `server`, `backlog` and `differential`.
