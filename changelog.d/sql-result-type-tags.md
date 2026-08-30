### A computed SQL column declared a type its own value could not be decoded as

Seven wire-level divergences on the PostgreSQL interface, all one shape: the
server computed the **right value**, declared a type taken from the **wrong
operand**, and the *client* raised while decoding it. Found by probing our PG
server against a live PostgreSQL 14 and comparing the result OID as well as the
value — an in-process comparison of the same statements showed nothing, because
the values were never wrong.

#### Fixed

- **`jsonb || jsonb` and `jsonb - key` lost the jsonb type.** `||` typed as
  `text` and `-` typed from its right operand (`int4` / `numeric`), so a jsonb
  payload went out under a numeric OID. Every `jsonb - anything` was a hard
  client-side failure — psycopg raised
  `invalid literal for int() with base 10: '{1,3}'` — and `||` answered a PG
  array literal (`{1,2,3}`) instead of jsonb (`[1, 2, 3]`). `#-`
  (`JSONBDeleteAtPath`) was already correct. Result: 13 shapes now match PG
  byte-for-byte, OID included.
- **An `unknown` literal widened instead of resolving to the other operand's
  type.** Postgres coerces an untyped literal to the *other* operand's type
  before choosing an operator, so that type decides both the parse and the
  error. `'1.5' + 1` is **integer** input and fails `22P02`; we answered `2.5`
  under the `int4` OID the literal `1` had already fixed, and psycopg raised.
  The `22P02` message now names the target type, as PG's does.
- **A date-shaped literal did date arithmetic.** `'2020-01-01' + 1` is integer
  input in PG (`22P02`); we answered `'2020-01-02'` under an `int4` OID — again
  a client-side crash. Only a bare *literal* is judged: a `::date` cast or a
  date column keeps its date arithmetic.
- **Beside an interval, the unknown literal must become an interval.**
  `'2020-01-01' + interval '1 day'` is `22007` in PG; we read the literal as a
  date and answered a timestamp under the *interval* OID, so psycopg raised
  `can't parse interval '2020-01-02 00:00:00'`. Restricted to `+` / `-`: for
  `*` / `/` PG resolves the unknown to a number instead, so
  `interval '1 day' * '2'` is still two days.
- **Boolean arithmetic was accepted.** Postgres defines no arithmetic operator
  on `boolean`, but Python's `bool` *is* an `int`, so nothing raised:
  `true + 1` quietly answered `2` and `true - false` answered `1`. Both now
  answer `42883`.

`tests/test_sql_result_type_tags.py` pins all of it over the real wire,
asserting the declared OID alongside the value, and compares 20 shapes against
a live PostgreSQL when one is reachable.
