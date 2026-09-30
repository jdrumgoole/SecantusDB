DROP VIEW IF EXISTS vgd_v
DROP TABLE IF EXISTS vgd_t
CREATE TABLE vgd_t (id int, name text, amt numeric)
CREATE VIEW vgd_v AS SELECT id, name FROM vgd_t WHERE amt > 10
