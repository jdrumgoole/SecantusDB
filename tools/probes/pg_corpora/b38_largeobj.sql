# reference-version: 15
# Large objects called from SQL (the Fastpath path is pgjdbc's).
SELECT lo_create(38001)
SELECT lo_create(38001)
SELECT lo_put(38001, 0, '\x41424344'::bytea)
SELECT lo_get(38001)
SELECT lo_get(38001, 1, 2)
SELECT lo_put(38001, 6, '\x45'::bytea)
SELECT lo_get(38001)
SELECT lo_get(38009)
BEGIN
SELECT lo_open(38001, 262144)
SELECT loread(0, 3)
SELECT lo_lseek(0, 1, 0)
SELECT loread(0, 100)
SELECT lo_tell(0)
SELECT lowrite(lo_open(38001, 131072), '\x5a5a'::bytea)
SELECT lowrite(0, '\x00'::bytea)
ROLLBACK
SELECT lo_get(38001)
BEGIN
SELECT lowrite(lo_open(38001, 131072), '\x5a5a'::bytea)
SELECT lo_truncate(1, 3)
COMMIT
SELECT lo_get(38001)
SELECT lo_from_bytea(38002, '\xdeadbeef'::bytea)
SELECT lo_get(38002)
SELECT lo_unlink(38001), lo_unlink(38002)
SELECT lo_unlink(38001)
SELECT lo_get(38001)
SELECT lo_create(38003)
SELECT oid FROM pg_largeobject_metadata WHERE oid BETWEEN 38001 AND 38003 ORDER BY oid
SELECT lo_unlink(38003)
SELECT count(*) FROM pg_largeobject_metadata WHERE oid BETWEEN 38001 AND 38003
