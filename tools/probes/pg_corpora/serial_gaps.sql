# A sequence advance is never rolled back: a statement that fails, and a
# block rolled back, still consume the values they drew.
BEGIN
SELECT nextval('sgp_s')
ROLLBACK
SELECT nextval('sgp_s')
INSERT INTO sgp (u) VALUES ('a')
INSERT INTO sgp (u) VALUES ('a')
INSERT INTO sgp (u) VALUES ('b') RETURNING id
INSERT INTO sgp (u) VALUES ('e'), ('e')
INSERT INTO sgp (u) VALUES ('f') RETURNING id
# A folded default takes the column's modifier as it is stored.
INSERT INTO sgd (id) VALUES (1)
ALTER TABLE sgd ADD COLUMN m numeric(5,2) DEFAULT 2.345
INSERT INTO sgd (id) VALUES (2)
SELECT * FROM sgd ORDER BY id
SELECT column_name, column_default FROM information_schema.columns WHERE table_name = 'sgd' ORDER BY ordinal_position
DROP TABLE sgp
DROP TABLE sgd
DROP SEQUENCE sgp_s
