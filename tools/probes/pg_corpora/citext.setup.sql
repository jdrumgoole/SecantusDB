DROP TABLE IF EXISTS ct33_t, ct33_u, ct33_p, ct33_j, ct33_a
DROP EXTENSION IF EXISTS citext
CREATE EXTENSION citext
CREATE TABLE ct33_t (id int, name citext)
INSERT INTO ct33_t VALUES (1, 'Apple'), (2, 'apple'), (3, 'BANANA'), (4, 'banana'), (5, 'Cherry'), (6, NULL), (7, 'aPPle'), (8, 'b'), (9, 'B'), (10, '_x')
CREATE TABLE ct33_u (name citext UNIQUE)
INSERT INTO ct33_u VALUES ('Hello')
CREATE TABLE ct33_p (k citext PRIMARY KEY, v int)
INSERT INTO ct33_p VALUES ('Key', 1), ('Other', 2)
CREATE TABLE ct33_j (name citext, color text)
INSERT INTO ct33_j VALUES ('APPLE', 'red'), ('banana', 'yellow')
