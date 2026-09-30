COMMENT ON TABLE cm_t IS 'the table'
COMMENT ON COLUMN cm_t.a IS 'col a'
COMMENT ON INDEX cm_i IS 'idx'
COMMENT ON SEQUENCE cm_s IS 'seq'
COMMENT ON VIEW cm_v IS 'view'
COMMENT ON CONSTRAINT cm_chk ON cm_t IS 'chk'
COMMENT ON CONSTRAINT cm_t_pkey ON cm_t IS 'pk'
COMMENT ON SCHEMA public IS 'pub'
SELECT obj_description('cm_t'::regclass, 'pg_class'), obj_description('cm_t'::regclass), col_description('cm_t'::regclass, 2), col_description('cm_t'::regclass, 1)
SELECT obj_description('cm_i'::regclass, 'pg_class')
SELECT a.attname, col_description(a.attrelid, a.attnum) FROM pg_attribute a WHERE a.attrelid = 'cm_t'::regclass AND a.attnum > 0 ORDER BY a.attnum
SELECT c.relname, obj_description(c.oid, 'pg_class') FROM pg_class c WHERE c.relname IN ('cm_t', 'cm_i') ORDER BY 1
COMMENT ON TABLE cm_t IS NULL
SELECT obj_description('cm_t'::regclass, 'pg_class') IS NULL
COMMENT ON TABLE nosuch IS 'x'
COMMENT ON COLUMN cm_t.nosuch IS 'x'
COMMENT ON CONSTRAINT nope ON cm_t IS 'x'
COMMENT ON INDEX nosuch_i IS 'x'
COMMENT ON TABLE cm_t IS 'again'
COMMENT ON TABLE cm_t IS ''
SELECT obj_description('cm_t'::regclass, 'pg_class') IS NULL
COMMENT ON COLUMN cm_t.a IS NULL
SELECT col_description('cm_t'::regclass, 2) IS NULL
INSERT INTO cm_t VALUES (1, 1, 'x')
SELECT * FROM cm_t
