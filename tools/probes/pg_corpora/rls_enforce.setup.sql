DROP TABLE IF EXISTS rle
DROP ROLE IF EXISTS rle_alice
DROP ROLE IF EXISTS rle_bob
DROP ROLE IF EXISTS rle_carol
CREATE ROLE rle_alice LOGIN
CREATE ROLE rle_bob LOGIN
CREATE ROLE rle_carol LOGIN BYPASSRLS
CREATE TABLE rle (id int, owner text, lvl int)
INSERT INTO rle VALUES (1, 'rle_alice', 1), (2, 'rle_bob', 2), (3, 'rle_alice', 3), (4, 'rle_bob', 1)
GRANT ALL ON rle TO rle_alice, rle_bob, rle_carol
ALTER TABLE rle ENABLE ROW LEVEL SECURITY
