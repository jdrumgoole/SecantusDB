CREATE TABLE tl_a (LIKE tl_src)
SELECT column_name, data_type, is_nullable, column_default FROM information_schema.columns WHERE table_name = 'tl_a' ORDER BY ordinal_position
INSERT INTO tl_a (id, name, n) VALUES (1, 'a', -1)
SELECT * FROM tl_a
CREATE TABLE tl_b (LIKE tl_src INCLUDING ALL, extra int)
SELECT column_name, is_nullable, column_default, is_generated FROM information_schema.columns WHERE table_name = 'tl_b' ORDER BY ordinal_position
INSERT INTO tl_b (id, n) VALUES (1, -1)
INSERT INTO tl_b (id, n) VALUES (1, 2)
INSERT INTO tl_b (id, n) VALUES (1, 3)
SELECT id, name, n, g FROM tl_b
CREATE TABLE tl_c (LIKE tl_src INCLUDING DEFAULTS)
INSERT INTO tl_c (id) VALUES (5)
SELECT id, name FROM tl_c
CREATE TABLE tl_d (LIKE nosuch)
