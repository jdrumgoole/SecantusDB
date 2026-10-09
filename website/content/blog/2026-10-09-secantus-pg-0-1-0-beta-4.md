Title: secantus-pg 0.1.0-beta.4 fixes a wrong result from prepared statements
Date: 2026-10-09 09:00:00
Slug: secantus-pg-0-1-0-beta-4
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-pg 0.1.0-beta.4 fixes a prepared statement that could return the wrong row in 0.1.0-beta.3, holds smallint to its range, and reuses the plan of a prepared INSERT or UPDATE.

If you run `secantusd-pg` 0.1.0-beta.3, upgrade. That release could return the
wrong row from a prepared statement whose parameter sits inside an expression
in the `WHERE` clause:

```sql
SELECT v FROM t WHERE k = $1 % 100000
```

The server keeps the plan of a prepared statement and substitutes each
execution's parameters into it. The planner computes `$1 % 100000` once and
puts the result in the plan, and beta.3 then substituted the raw parameter over
that result on later executions. An execution that planned the statement was
right, and one that reused the plan looked up the wrong key. There was no
error. A parameter
compared directly with a column, `WHERE k = $1`, was never affected, and that
is what most drivers and ORMs send.

In beta.4 a plan is reused only when every parameter stands alone where its
value is used: compared with a column in the `WHERE` (`k = $1`,
`k IN ($1, $2)`), an item of an `INSERT`'s `VALUES`, the whole of `SET v = $1`,
or one operand of `SET v = v + $1`. A parameter anywhere else, such as
`k = -$1`, `$1::int2`, `BETWEEN` or `LIMIT $1`, is planned on every execution.

`smallint` is now held to its range. `40000::smallint` answered 40000, and a
`smallint` column stored it; both are PostgreSQL's 22003 `smallint out of
range` now. A float outside `integer`'s range, or NaN, cast to an integer is
22003 where it used to saturate, and digits too large for the type
(`'99999999999'::int`) are 22003 where they were 22P02.

The release also makes prepared writes cheaper. A plan used to be reused only
when its parameters landed in a `WHERE` clause, so a prepared `INSERT` and an
`UPDATE ... SET v = $1` were planned again on every execution. They are reused
now when the value goes into an `int4`, `int8`, `float8` or `text` column from
a parameter of the matching kind. A column with a declared width, a narrower
range, a domain, an enum or a generated value is still planned per execution,
so its checks see every value.

We measured this release on Linux for the first time: a 16-vCPU DigitalOcean
droplet, with PostgreSQL 16 at its defaults beside it. Counting CPU
instructions in the server per prepared statement, against beta.3 on the same
machine, `UPDATE ... SET v = v + 1` went from 592k to 430k, `SET v = $1` from
457k to 353k, and `INSERT` from 366k to 294k. A row read by primary key is
unchanged at 148k.

The server is still well behind PostgreSQL on durable writes there. With one,
two, four and eight clients each updating a row by primary key, it did 2,396,
4,218, 6,810 and 9,525 statements a second; PostgreSQL 16 in the same run did
5,010, 8,434, 15,202 and 22,976. That is 2.0 to 2.4 times slower, where beta.3
was 2.3 to 2.6 times slower. Both servers do one log write and one sync per
commit, so the gap is CPU work in our write path. Inserts on that benchmark did
not move. It binds a small Python integer into a `bigint` column, psycopg sends
that as a `smallint`, and a value the planner has to widen is not reused yet.
On macOS, where the last post's numbers came from, a durable `UPDATE` was level
with PostgreSQL before this release.

This is a server for tests. It is single-node, it is a beta, and a role with
no password connects without one, so keep it on loopback.

`cargo install secantus-pg --version 0.1.0-beta.4` builds it from crates.io.
Binaries for Linux x86_64 and macOS arm64 are on the release.

[Rust PostgreSQL server](https://secantusdb.com/rust-pg.html) ·
[secantus-pg on crates.io](https://crates.io/crates/secantus-pg) ·
[PostgreSQL binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusd-pg-v0.1.0-beta.4)
