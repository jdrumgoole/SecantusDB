SELECT row_to_json(t) FROM (SELECT 1 AS a, 'b' AS b) t
SELECT row_to_json(t) FROM (SELECT id, a FROM jr WHERE id=1) t
SELECT to_json(r) FROM (SELECT 1 AS a) r
SELECT row_to_json(jr) FROM jr ORDER BY id
SELECT jr FROM jr WHERE id=1
SELECT (jr)::text FROM jr WHERE id=1
SELECT id, a FROM jr ORDER BY id
