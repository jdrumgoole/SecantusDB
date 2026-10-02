# reference-version: 15
# macaddr / macaddr8 / pg_lsn / txid_snapshot / pg_snapshot / xid / xid8 /
# cid: input forms, output, ordering, casts between them, pg_type rows.
SELECT '08-00-2B-01-02-03'::macaddr, '0800.2b01.0203'::macaddr, '08002b:010203'::macaddr, '08002b010203'::macaddr
SELECT '08002b010203'::macaddr8, '08:00:2b:01:02:03'::macaddr8, '0800.2b01.0203.0405'::macaddr8
SELECT '08:00:2b:01:02'::macaddr
SELECT '08:00-2b:01:02:03'::macaddr8
SELECT '08:00:2b:ff:fe:01:02:03'::macaddr8::macaddr, '08:00:2b:01:02:03'::macaddr::macaddr8
SELECT '08:00:2b:01:02:03:04:05'::macaddr8::macaddr
SELECT '16/B374D848'::pg_lsn, '0/0'::pg_lsn, '00000016/0000000A'::pg_lsn, pg_typeof('1/1'::pg_lsn)
SELECT '16/'::pg_lsn
SELECT '1/1'::pg_lsn - '1/0'::pg_lsn, '1/1'::pg_lsn > '0/FFFFFFFF'::pg_lsn
SELECT '10:20:10,14,14,15'::txid_snapshot, '10:20:'::pg_snapshot
SELECT '0:20:'::pg_snapshot
SELECT '10:20:25'::txid_snapshot
SELECT '123'::xid, '-1'::xid, '-1'::xid8, ' 0x10'::cid, pg_typeof('1'::xid)
SELECT m, m8, l FROM b40_mac ORDER BY l
SELECT m, m8 FROM b40_mac ORDER BY m NULLS FIRST
SELECT l::text, l FROM b40_mac WHERE l > '9/0' ORDER BY l DESC
SELECT max(l) FROM b40_mac
SELECT '{08:00:2b:01:02:03,NULL}'::macaddr[], '{1/2,3/4}'::pg_lsn[], '{1/2}'::pg_lsn[]::text, array_agg(l ORDER BY l) FROM b40_mac
SELECT typname, oid, typarray FROM pg_type WHERE typname IN ('macaddr', 'macaddr8', 'pg_lsn', 'txid_snapshot', 'pg_snapshot', 'xid', 'xid8', 'cid', '_macaddr', '_pg_lsn') ORDER BY oid
# A LIKE pattern ending in its escape character errors only when matching
# reaches it; over an indexed catalog name column the planner's exact
# prefix never does.
SELECT 'a' LIKE 'a\', 'b' LIKE 'a\'
SELECT 'ab' LIKE 'a\'
SELECT 'xab' LIKE '%a\'
SELECT 'xa' LIKE '%a\'
SELECT t FROM b40_lk WHERE t LIKE 'b\' ORDER BY t
SELECT t FROM b40_lk WHERE t LIKE 'a\' ORDER BY t
SELECT t FROM b40_lk WHERE t NOT LIKE 'c\' ORDER BY t
SELECT relname FROM pg_class WHERE relname LIKE 'a\'
SELECT relname FROM pg_class WHERE relname::text LIKE 'a\'
# A user type named like another's array renames that array, as
# makeArrayTypeName does.
SELECT typname, typarray::regtype FROM pg_type WHERE typname LIKE '%b40_custom' ORDER BY typname
SELECT attname, t.typname FROM pg_attribute a JOIN pg_type t ON t.oid = a.atttypid WHERE attrelid = 'b40_ct'::regclass AND attnum > 0 ORDER BY attnum
# A two-dimensional enum array.
SELECT '{{duplicate,new},{spike,spike}}'::b40_flag[][]
SELECT ('{{duplicate,new},{spike,spike}}'::b40_flag[])[2][1]
# A BC reading that names its own offset is that instant.
SET timezone = 'UTC'
SELECT '0101-01-01 BC -05'::timestamptz, '0101-01-01 00:00 BC -05'::timestamptz, '0001-01-01 23:00 BC -05'::timestamptz
SELECT '0001-12-31 BC +12'::timestamptz, '20000-01-01 +03'::timestamptz
SET timezone = 'America/New_York'
SELECT '0101-01-01 BC -05'::timestamptz, '0101-01-01 BC +03'::timestamptz, '0101-01-01 BC America/Los_Angeles'::timestamptz
# SHOW timezone gives a zone file's own case, and upper-cases a POSIX spec.
SET timezone = 'gmt-3'
SHOW timezone
SET timezone = 'america/new_york'
SHOW timezone
SET timezone = 'est5edt'
SHOW timezone
SET timezone = 'utc'
SHOW timezone
RESET timezone
# System relations carry initdb's ACL.
SELECT relacl FROM pg_class WHERE relname = 'pg_class'
SELECT relacl FROM pg_class WHERE relname = 'pg_authid'
SELECT relacl FROM pg_class WHERE relname = 'pg_settings'
SELECT relacl FROM pg_class WHERE relname = 'tables' AND relnamespace = 'information_schema'::regnamespace
SELECT relacl FROM pg_class WHERE relname = 'pg_class_oid_index'
DROP TABLE b40_ct
DROP TYPE _b40_custom
DROP TYPE b40_custom
DROP TYPE b40_flag
DROP TABLE b40_lk
DROP TABLE b40_mac
