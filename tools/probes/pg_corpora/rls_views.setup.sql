RESET ROLE
DROP VIEW IF EXISTS rv_v CASCADE
DROP VIEW IF EXISTS rv_vb CASCADE
DROP VIEW IF EXISTS rv_vc CASCADE
DROP TABLE IF EXISTS rv_t CASCADE
DROP ROLE IF EXISTS rv_alice
DROP ROLE IF EXISTS rv_bob
DROP ROLE IF EXISTS rv_carol
CREATE ROLE rv_alice
CREATE ROLE rv_bob
CREATE ROLE rv_carol BYPASSRLS
CREATE TABLE rv_t (id int, who text)
INSERT INTO rv_t VALUES (1, 'rv_alice'), (2, 'rv_bob'), (3, 'rv_carol')
ALTER TABLE rv_t ENABLE ROW LEVEL SECURITY
CREATE POLICY p ON rv_t USING (who = current_user)
CREATE POLICY pb ON rv_t TO rv_bob USING (id < 3)
GRANT SELECT ON rv_t TO rv_alice, rv_bob, rv_carol
CREATE VIEW rv_v AS SELECT * FROM rv_t
GRANT SELECT ON rv_v TO rv_alice, rv_bob
CREATE VIEW rv_vb AS SELECT * FROM rv_t
ALTER VIEW rv_vb OWNER TO rv_bob
GRANT SELECT ON rv_vb TO rv_alice
CREATE VIEW rv_vc AS SELECT * FROM rv_t
ALTER VIEW rv_vc OWNER TO rv_carol
GRANT SELECT ON rv_vc TO rv_alice
