# reference-version: 15
# The snapshot functions; set_config(..., true) outside a block lasts for
# the statement; large-object descriptors close with the implicit
# transaction and number from 0 again.
SELECT pg_snapshot_xmin('10:20:10,14,15'::pg_snapshot), pg_snapshot_xmax('10:20:10,14,15'::pg_snapshot)
SELECT pg_typeof(pg_snapshot_xmin('10:20:'::pg_snapshot)), pg_typeof(txid_snapshot_xmin('10:20:'))
SELECT pg_snapshot_xip('10:20:10,14,15'::pg_snapshot)
SELECT x FROM pg_snapshot_xip('10:20:10,14,15'::pg_snapshot) AS x ORDER BY 1 DESC
SELECT pg_snapshot_xip('10:10:'::pg_snapshot)
SELECT pg_visible_in_snapshot('9'::xid8, '10:20:10,14,15'), pg_visible_in_snapshot('14'::xid8, '10:20:10,14,15'), pg_visible_in_snapshot('12'::xid8, '10:20:10,14,15'), pg_visible_in_snapshot('25'::xid8, '10:20:10,14,15')
SELECT txid_snapshot_xmin('10:20:10,14,15'), txid_snapshot_xmax('10:20:10,14,15'), txid_visible_in_snapshot(12, '10:20:10,14,15'), txid_visible_in_snapshot(20, '10:20:10,14,15')
SELECT txid_snapshot_xip('10:20:10,14,15')
SELECT pg_snapshot_xmin(NULL), pg_visible_in_snapshot(NULL, '10:20:')
SELECT pg_typeof(pg_current_snapshot()), pg_snapshot_xmin(pg_current_snapshot()) <= pg_snapshot_xmax(pg_current_snapshot())
SELECT pg_typeof(txid_current_snapshot())
SELECT pg_snapshot_xmin('0:20:'::pg_snapshot)
SELECT set_config('b42.v', 'a', false)
SELECT set_config('b42.v', 'b', true), current_setting('b42.v')
SELECT current_setting('b42.v')
SELECT current_setting('b42.v'), set_config('b42.v', 'c', true), current_setting('b42.v')
SELECT current_setting('b42.v')
BEGIN
SELECT set_config('b42.v', 'd', true)
SELECT current_setting('b42.v')
COMMIT
SELECT current_setting('b42.v')
SELECT lo_from_bytea(424242, 'hello')
SELECT lo_open(424242, 262144)
SELECT loread(0, 10)
BEGIN
SELECT lo_open(424242, 262144)
SELECT loread(0, 3)
COMMIT
SELECT loread(0, 3)
SELECT lo_open(424242, 262144), loread(0, 2)
SELECT lo_unlink(424242)
