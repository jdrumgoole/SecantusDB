DROP TABLE IF EXISTS sppub_t CASCADE
DROP SEQUENCE IF EXISTS sppub_s
CREATE TABLE sppub_t (k int primary key, t text)
INSERT INTO sppub_t VALUES (1, 'a'), (2, 'b'), (3, 'c')
CREATE VIEW sppub_v AS SELECT t FROM sppub_t
CREATE VIEW sppub_w AS SELECT t FROM sppub_v WHERE t < 'c'
CREATE SEQUENCE sppub_s
