# ALTER VIEW: OWNER TO, RENAME TO / RENAME COLUMN (dependent views follow),
# column defaults for INSERT through the view, SET / RESET options.
ALTER VIEW av_v OWNER TO av_bob
SELECT viewname, viewowner = current_user FROM pg_views WHERE viewname LIKE 'av\_%' ORDER BY 1
SELECT * FROM av_v
ALTER VIEW av_v OWNER TO CURRENT_USER
ALTER VIEW av_v RENAME TO av_w
SELECT * FROM av_w
SELECT * FROM av_vv
ALTER VIEW av_w RENAME COLUMN v TO vv
SELECT * FROM av_w
ALTER VIEW av_w RENAME COLUMN nope TO x
ALTER VIEW av_w RENAME COLUMN vv TO id
ALTER VIEW av_w ALTER COLUMN vv SET DEFAULT 7
INSERT INTO av_w (id) VALUES (2)
INSERT INTO av_w VALUES (3, DEFAULT)
INSERT INTO av_w (id) SELECT 4
SELECT * FROM av_t ORDER BY id
ALTER VIEW av_w ALTER COLUMN vv DROP DEFAULT
INSERT INTO av_w (id) VALUES (5)
SELECT * FROM av_t ORDER BY id
ALTER VIEW av_w ALTER COLUMN nope SET DEFAULT 1
ALTER VIEW av_w SET (check_option = local)
INSERT INTO av_vv VALUES (-1, 0)
ALTER VIEW av_vv SET (check_option = cascaded)
INSERT INTO av_vv VALUES (-2, 0)
ALTER VIEW av_vv RESET (check_option)
INSERT INTO av_vv VALUES (-3, 0)
ALTER VIEW av_w SET (check_option = sideways)
ALTER VIEW av_w SET (security_barrier)
ALTER VIEW av_w SET (security_barrier = maybe)
ALTER VIEW av_w SET (bogus = 1)
ALTER VIEW IF EXISTS nope OWNER TO av_bob
ALTER VIEW nope OWNER TO av_bob
ALTER VIEW av_t OWNER TO av_bob
ALTER VIEW av_w OWNER TO nobody_here
ALTER TABLE av_w RENAME TO av_v
SELECT relname, relkind FROM pg_class WHERE relname LIKE 'av\_%' ORDER BY 1
SELECT * FROM av_vv ORDER BY id
