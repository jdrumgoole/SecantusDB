# DROP TABLE ... CASCADE: the views over the table (transitively) and the
# foreign keys referencing it go with it; RESTRICT names them.
DROP TABLE dc_p
DROP TABLE dc_p RESTRICT
DROP TABLE dc_p CASCADE
SELECT count(*) FROM information_schema.views WHERE table_name LIKE 'dc\_%'
SELECT conname FROM pg_constraint WHERE conrelid IN ('dc_c'::regclass, 'dc_c2'::regclass)
INSERT INTO dc_c VALUES (99)
SELECT * FROM dc_c ORDER BY x
DROP TABLE dc_s
DROP TABLE dc_c, dc_c2 CASCADE
DROP TABLE IF EXISTS dc_nope CASCADE
