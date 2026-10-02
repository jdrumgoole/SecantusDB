# reference-version: 15
# PL/pgSQL OPEN: a refcursor the caller FETCHes from; pg_cursors lists it.
BEGIN
SELECT b38_get()
FETCH ALL IN "<unnamed portal 1>"
SELECT b38_out(2)
FETCH ALL IN "<unnamed portal 2>"
SELECT b38_named('b38cur')
FETCH 1 IN b38cur
FETCH BACKWARD 1 IN b38cur
SELECT name, statement, is_holdable, is_binary, is_scrollable FROM pg_cursors ORDER BY name
SELECT b38_bound()
FETCH ALL IN c
CLOSE c
SELECT b38_named('b38cur')
ROLLBACK
SELECT count(*) FROM pg_cursors
SELECT b38_named(NULL) IS NOT NULL
DROP FUNCTION b38_get()
DROP FUNCTION b38_out(int4)
DROP FUNCTION b38_named(refcursor)
DROP FUNCTION b38_bound()
DROP TABLE b38_rc
