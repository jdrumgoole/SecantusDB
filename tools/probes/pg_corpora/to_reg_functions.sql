# reference-version: 15
# to_regnamespace / to_regrole / to_regcollation / to_regproc / to_regprocedure: the reg* cast, NULL where it says the object does not exist.
SELECT to_regnamespace('public')::oid, to_regnamespace('nope'), to_regrole(current_user::text) IS NOT NULL, to_regrole('nope')
SELECT to_regcollation('"sv-x-icu"')::oid, to_regcollation('nope'), to_regcollation('C'), to_regcollation('"C"')::oid
SELECT to_regproc('abs'), to_regproc('now')::text, to_regproc('nope'), to_regprocedure('nope(int4)')
SELECT pg_typeof(to_regnamespace('public')), pg_typeof(to_regrole('x')), pg_typeof(to_regcollation('x')), pg_typeof(to_regproc('x')), pg_typeof(to_regprocedure('x()'))
SELECT n.nspname FROM pg_namespace n WHERE n.oid = to_regnamespace('pg_catalog')
SELECT to_regproc(NULL), to_regnamespace(NULL)
SELECT to_regnamespace('a.b')
SELECT to_regrole('a.b')
SELECT to_regnamespace('"a.b"')
SELECT 'a.b.c.d'::regnamespace::text
