# Extended query protocol: every line runs as text params, a server-side
# PREPARED statement, and with BINARY result format. Divergences are labelled
# with the binding mode, because two known bugs here were binary-only.
# --- scalar round-trips through Bind
SELECT %s::int ||| [5]
SELECT %s::int8 ||| [100000000000]
SELECT %s::float8 ||| [1.25]
SELECT %s::numeric ||| [Decimal("1.50")]
SELECT %s::bool ||| [True]
SELECT %s::text ||| ["alpha"]
SELECT %s::date ||| [datetime.date(2020,1,5)]
SELECT %s::timestamp ||| [datetime.datetime(2020,1,5,10,20,30)]
SELECT %s::timestamptz ||| [datetime.datetime(2020,1,5,10,20,30,tzinfo=datetime.timezone.utc)]
SELECT %s::bytea ||| [b"AB"]
SELECT %s::int[] ||| [[1,2,3]]
SELECT %s::jsonb ||| ['{"a": 1}']
SELECT %s::text ||| [None]
SELECT %s::int ||| [None]
SELECT %s::interval ||| [datetime.timedelta(days=1, hours=2)]
SELECT %s::time ||| [datetime.time(10,20,30)]
# --- parameters in predicates (the pushdown path)
SELECT id FROM p20 WHERE n = %s ORDER BY id ||| [10]
SELECT id FROM p20 WHERE s = %s ORDER BY id ||| ["alpha"]
SELECT id FROM p20 WHERE d = %s ORDER BY id ||| [datetime.date(2020,1,5)]
SELECT id FROM p20 WHERE x = %s ORDER BY id ||| [Decimal("1.50")]
SELECT id FROM p20 WHERE b = %s ORDER BY id ||| [True]
SELECT id FROM p20 WHERE n > %s ORDER BY id ||| [5]
SELECT id FROM p20 WHERE s IS NOT DISTINCT FROM %s ORDER BY id ||| [None]
SELECT id FROM p20 WHERE n = ANY(%s) ORDER BY id ||| [[10, 99]]
SELECT id FROM p20 WHERE arr @> %s ORDER BY id ||| [[1,2]]
SELECT id FROM p20 WHERE j @> %s::jsonb ORDER BY id ||| ['{"a": 1}']
SELECT id FROM p20 WHERE s LIKE %s ORDER BY id ||| ["al%"]
SELECT id FROM p20 WHERE ts < %s ORDER BY id ||| [datetime.datetime(2021,1,1)]
# --- parameters in the select list and expressions
SELECT n + %s FROM p20 WHERE id=1 ||| [5]
SELECT s || %s FROM p20 WHERE id=1 ||| ["!"]
SELECT coalesce(s, %s) FROM p20 ORDER BY id ||| ["fallback"]
SELECT %s::int + %s::int ||| [2, 3]
SELECT upper(%s) ||| ["abc"]
SELECT count(*) FILTER (WHERE n > %s) FROM p20 ||| [5]
SELECT CASE WHEN n > %s THEN 'hi' ELSE 'lo' END FROM p20 ORDER BY id ||| [5]
# --- parameters in LIMIT / OFFSET and ORDER BY position
SELECT id FROM p20 ORDER BY id LIMIT %s ||| [1]
SELECT id FROM p20 ORDER BY id OFFSET %s ||| [1]
# --- DML with parameters and RETURNING
INSERT INTO p20 (id, s, n) VALUES (%s, %s, %s) RETURNING id, s, n ||| [10, "ten", 10]
UPDATE p20 SET n = %s WHERE id = %s RETURNING id, n ||| [11, 10]
DELETE FROM p20 WHERE id = %s RETURNING id ||| [10]
# --- reading every column back, which is where BINARY output is exercised
SELECT id, s, n, big, x, f, b FROM p20 ORDER BY id
SELECT d, ts, tz FROM p20 ORDER BY id
SELECT by, arr, j FROM p20 ORDER BY id
SELECT id FROM p20 WHERE id = %s ||| [1]
