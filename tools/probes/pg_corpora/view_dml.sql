INSERT INTO vd_v (id, grp, n) VALUES (4, 'c', 40) RETURNING id, grp, dbl
INSERT INTO vd_v VALUES (5, 'c', 5)
SELECT id, g, n FROM vd_t ORDER BY id
INSERT INTO vd_v (id, dbl) VALUES (6, 1)
INSERT INTO vd_v (id, grp) VALUES (7, 'd')
SELECT id, g, n FROM vd_t WHERE id = 7
UPDATE vd_v SET n = n + 1 WHERE grp = 'a' RETURNING id, n, dbl
SELECT id, n FROM vd_t ORDER BY id
UPDATE vd_v SET dbl = 0
UPDATE vd_v v SET grp = 'z' WHERE v.dbl > 60 RETURNING v.id, v.grp
SELECT id, g FROM vd_t ORDER BY id
DELETE FROM vd_v WHERE n < 25 RETURNING id
SELECT id FROM vd_t ORDER BY id
UPDATE vd_vv SET grp = 'y' RETURNING id
SELECT id, g FROM vd_t ORDER BY id
DELETE FROM vd_vv
INSERT INTO vd_agg VALUES ('q', 1)
UPDATE vd_agg SET c = 0
DELETE FROM vd_agg
SELECT count(*) FROM vd_t
INSERT INTO vd_chk VALUES (20, 5)
INSERT INTO vd_chk VALUES (21, -5)
INSERT INTO vd_chk VALUES (22, NULL)
UPDATE vd_chk SET n = -1 WHERE id = 20
UPDATE vd_chk SET n = 6 WHERE id = 20 RETURNING id, n
INSERT INTO vd_cas2 VALUES (30, 50)
INSERT INTO vd_cas2 VALUES (31, 500)
INSERT INTO vd_cas2 VALUES (32, 1)
INSERT INTO vd_loc2 VALUES (33, 500)
INSERT INTO vd_loc2 VALUES (34, 1)
SELECT id, n FROM vd_t WHERE id >= 20 ORDER BY id
INSERT INTO vd_cols VALUES (40, 'cols') RETURNING k, label
UPDATE vd_cols SET label = 'COLS' WHERE k = 40 RETURNING label
DELETE FROM vd_cols WHERE k = 40 RETURNING k
