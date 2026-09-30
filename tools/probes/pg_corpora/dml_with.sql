WITH moved AS (DELETE FROM dw_src WHERE id = 1 RETURNING id, v) INSERT INTO dw_arch SELECT id, v FROM moved
SELECT * FROM dw_arch
SELECT id FROM dw_src ORDER BY id
WITH ins AS (INSERT INTO dw_ids (name) VALUES ('x'), ('y') RETURNING id, name) SELECT name, id FROM ins ORDER BY id
WITH u AS (UPDATE dw_src SET v = upper(v) RETURNING id, v) SELECT count(*), max(v) FROM u
SELECT v FROM dw_src ORDER BY id
WITH d AS (DELETE FROM dw_arch RETURNING *) SELECT count(*) FROM d
SELECT dw_add('z'), dw_add('w')
SELECT name FROM dw_ids ORDER BY id
SELECT dw_upd(2)
SELECT dw_upd(99)
SELECT dw_strict(99)
SELECT dw_strict(3)
WITH n AS (INSERT INTO dw_ids (name) VALUES ('q')) SELECT 1
SELECT count(*) FROM dw_ids WHERE name = 'q'
