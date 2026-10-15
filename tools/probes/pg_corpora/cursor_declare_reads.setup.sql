DROP TABLE IF EXISTS b5c_st
DROP TABLE IF EXISTS b5c_su
DROP TABLE IF EXISTS b5c_col
DROP TABLE IF EXISTS b5c_rls
DROP ROLE IF EXISTS b5c_r
CREATE ROLE b5c_r
CREATE TABLE b5c_st (a int PRIMARY KEY, b text)
INSERT INTO b5c_st SELECT g, g::text FROM generate_series(1, 20) g
CREATE TABLE b5c_su (a int, c int)
INSERT INTO b5c_su VALUES (1, 1), (2, 2)
CREATE TABLE b5c_col (a int, b int)
INSERT INTO b5c_col VALUES (1, 2), (3, 4)
GRANT SELECT (a) ON b5c_col TO b5c_r
CREATE TABLE b5c_rls (a int, owner text)
INSERT INTO b5c_rls VALUES (1, 'b5c_r'), (2, 'other')
GRANT SELECT ON b5c_rls TO b5c_r
ANALYZE b5c_st
ANALYZE b5c_su
