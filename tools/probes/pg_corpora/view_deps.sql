# A view follows a rename of what it reads (PostgreSQL binds by OID), and its
# `*` is expanded when it is created.
ALTER TABLE vdp_t ADD COLUMN z int DEFAULT 9
SELECT * FROM vdp_star ORDER BY id
SELECT * FROM vdp_cols ORDER BY a
ALTER TABLE vdp_t RENAME TO vdp_t2
SELECT * FROM vdp_v ORDER BY id
SELECT * FROM vdp_vv ORDER BY id
SELECT * FROM vdp_star ORDER BY id
SELECT * FROM vdp_join ORDER BY id
SELECT * FROM vdp_sub ORDER BY id
ALTER TABLE vdp_t2 RENAME COLUMN v TO vee
SELECT * FROM vdp_v ORDER BY id
SELECT * FROM vdp_vv ORDER BY id
SELECT * FROM vdp_star ORDER BY id
SELECT * FROM vdp_join ORDER BY id
SELECT * FROM vdp_sub ORDER BY id
SELECT * FROM vdp_cols ORDER BY a
ALTER TABLE vdp_t2 RENAME COLUMN id TO ident
SELECT * FROM vdp_join ORDER BY id
SELECT * FROM vdp_sub ORDER BY id
ALTER TABLE vdp_u RENAME COLUMN w TO ww
SELECT * FROM vdp_join ORDER BY id
INSERT INTO vdp_v VALUES (7, 8)
INSERT INTO vdp_star VALUES (8, 9)
INSERT INTO vdp_cols VALUES (9, 10)
SELECT * FROM vdp_t2 ORDER BY ident
ALTER TABLE vdp_t2 RENAME TO vdp_t3
SELECT * FROM vdp_vv ORDER BY id
SELECT column_name FROM information_schema.columns WHERE table_name = 'vdp_star' ORDER BY ordinal_position
SELECT table_name, table_type FROM information_schema.tables WHERE table_name LIKE 'vdp\_%' ORDER BY 1
SELECT table_name, column_name, data_type, is_nullable FROM information_schema.columns WHERE table_name IN ('vdp_join', 'vdp_cols') ORDER BY table_name, ordinal_position
SELECT table_name, check_option, is_updatable, is_insertable_into FROM information_schema.views WHERE table_name LIKE 'vdp\_%' ORDER BY 1
