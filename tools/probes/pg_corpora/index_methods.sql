CREATE INDEX im_j ON im USING gin (j)
CREATE INDEX im_jp ON im USING gin (j jsonb_path_ops)
CREATE INDEX im_tags ON im USING gin (tags)
CREATE INDEX im_tv ON im USING gin (tv)
CREATE INDEX im_r ON im USING gist (r)
CREATE INDEX im_n ON im USING brin (n)
CREATE INDEX im_t ON im USING spgist (t)
CREATE INDEX im_bad1 ON im USING gin (n)
CREATE INDEX im_bad2 ON im USING gist (t)
CREATE UNIQUE INDEX im_bad3 ON im USING gin (j)
CREATE INDEX im_bad4 ON im USING gin (j DESC)
CREATE INDEX im_bad5 ON im USING gin (j nope_ops)
CREATE INDEX im_bad6 ON im USING nosuch (n)
CREATE INDEX im_bad7 ON im USING hash (n DESC)
CREATE INDEX im_tp ON im (t text_pattern_ops)
CREATE INDEX im_fts ON im USING gin (to_tsvector('english', t))
SELECT indexname, indexdef FROM pg_indexes WHERE tablename = 'im' ORDER BY 1
SELECT id FROM im WHERE j @> '{"a": 1}'
SELECT id FROM im WHERE tags @> '{y}' ORDER BY id
SELECT id FROM im WHERE r @> 3 ORDER BY id
DROP INDEX im_j
SELECT count(*) FROM pg_indexes WHERE tablename = 'im'
CREATE EXTENSION btree_gin
CREATE EXTENSION btree_gist
CREATE INDEX im_gn ON im USING gin (n)
CREATE INDEX im_gt ON im USING gist (t)
CREATE INDEX im_gj ON im USING gist (j)
SELECT indexname, indexdef FROM pg_indexes WHERE tablename = 'im' AND indexname LIKE 'im_g%' ORDER BY 1
DROP EXTENSION btree_gin
DROP EXTENSION btree_gin CASCADE
DROP EXTENSION btree_gist CASCADE
SELECT indexname FROM pg_indexes WHERE tablename = 'im' AND indexname LIKE 'im_g%' ORDER BY 1
