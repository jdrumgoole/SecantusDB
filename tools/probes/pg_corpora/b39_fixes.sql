# reference-version: 15
# ROLLBACK TO SAVEPOINT undoes a SET made after the savepoint (and RELEASE
# keeps it); a volatile argument of pg_typeof() runs once; there is no prefix
# minus or plus for money.
BEGIN
SET lock_timeout = '1s'
SAVEPOINT a
SET lock_timeout = '2s'
SHOW lock_timeout
ROLLBACK TO SAVEPOINT a
SHOW lock_timeout
SET LOCAL lock_timeout = '3s'
SAVEPOINT b
SET LOCAL lock_timeout = '4s'
ROLLBACK TO b
SHOW lock_timeout
SAVEPOINT c
SET lock_timeout = '5s'
RELEASE c
SHOW lock_timeout
ROLLBACK
SHOW lock_timeout
BEGIN
SAVEPOINT d
SET application_name = 'b39d'
ROLLBACK TO d
SHOW application_name
COMMIT
SELECT pg_typeof(nextval('b39_s'))
SELECT currval('b39_s')
SELECT pg_typeof(nextval('b39_s')), currval('b39_s')
SELECT length(nextval('b39_s')::text), currval('b39_s')
SELECT -'1'::money
SELECT +'1'::money
SELECT -m FROM b39_m
SELECT -('2'::money)
SELECT 1 WHERE -'1'::money < '0'
DROP TABLE b39_m
DROP SEQUENCE b39_s
SELECT 'a' LIKE %s, '_' LIKE %s, 'a' ~ %s ||| ['\\_', '\\_', 'a']
SELECT lseg '((-1,0),(1,0))' ?# box '((-2,-2),(2,2))', lseg '((5,5),(6,6))' ?# box '((-2,-2),(2,2))', lseg '((-3,0),(3,0))' ?# box '((-2,-2),(2,2))'
SELECT b39_lk()
SELECT b39_args(4, 'x')
SELECT count(*) > 400, count(*) FILTER (WHERE catcode = 'R') > 50 FROM pg_get_keywords()
SELECT word, catcode, barelabel, catdesc FROM pg_catalog.pg_get_keywords() WHERE word IN ('select', 'abort', 'between', 'authorization') ORDER BY word
SELECT setting, context, vartype FROM pg_catalog.pg_settings WHERE name IN ('max_index_keys', 'max_identifier_length', 'block_size') ORDER BY name
SHOW max_index_keys
SET max_index_keys = 3
SELECT typname, typtype FROM pg_type WHERE oid IN (2249, 2278, 2279, 2275) ORDER BY oid
SELECT b39_ins(7)
SELECT id FROM b39_ins_t
DROP FUNCTION b39_ins(int)
DROP TABLE b39_ins_t
DROP FUNCTION b39_lk()
DROP FUNCTION b39_args(int, text)
DROP TABLE b39_lt
