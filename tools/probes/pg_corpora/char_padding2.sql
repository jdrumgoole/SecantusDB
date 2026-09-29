SELECT id FROM bp2 WHERE c = 'x' ORDER BY id
SELECT id FROM bp2 WHERE c = 'y' ORDER BY id
SELECT id FROM bp2 WHERE c = 'y  ' ORDER BY id
SELECT id FROM bp2 WHERE c = 'z' ORDER BY id
SELECT id FROM bp2 WHERE v = 'y' ORDER BY id
SELECT id FROM bp2 WHERE v = 'y   ' ORDER BY id
SELECT id, c || '|', length(c), octet_length(c) FROM bp2 ORDER BY id
SELECT count(*) FROM bp2 WHERE c IN ('x', 'z')
SELECT id FROM bp2 WHERE c LIKE 'x%' ORDER BY id
SELECT id FROM bp2 WHERE c LIKE '%    ' ORDER BY id
SELECT id, c FROM bp2 ORDER BY c, id
SELECT max(c), min(c) FROM bp2
SELECT c, count(*) FROM bp2 GROUP BY c ORDER BY c
SELECT DISTINCT c FROM bp2 ORDER BY c
SELECT id FROM bp2 WHERE c > 'x' ORDER BY id
SELECT upper(c) || '|' FROM bp2 WHERE id = 1
SELECT c::varchar(3) || '|', c::text || '|' FROM bp2 WHERE id = 2
INSERT INTO bp2 VALUES (9, 'toolong', 'v')
INSERT INTO bp2 VALUES (9, 'ab      ', 'v')
SELECT c || '|' FROM bp2 WHERE id = 9
UPDATE bp2 SET v = v || 'xxxxxx' WHERE id = 1
UPDATE bp2 SET c = c || '       ' WHERE id = 1
SELECT c || '|', v || '|' FROM bp2 WHERE id = 1
