# reference-version: 15
# Batch 67: an unread LATERAL subquery output is not computed, so the sibling output only it read is not either.
SELECT count(*) FROM (SELECT a/b AS x FROM b67_l) s, LATERAL (SELECT s.x) l
SELECT count(*) FROM (SELECT a/b AS x, a FROM b67_l) s, LATERAL (SELECT s.x, s.a) l
SELECT s.a FROM (SELECT a/b AS x, a FROM b67_l) s, LATERAL (SELECT s.x AS y) l ORDER BY 1
SELECT l.z FROM (SELECT a/b AS x, a FROM b67_l) s, LATERAL (SELECT s.x AS y, s.a * 10 AS z) l ORDER BY 1
SELECT count(*) FROM (SELECT a/b AS x FROM b67_l) s CROSS JOIN LATERAL (SELECT s.x) l
SELECT count(*) FROM (SELECT a/b AS x FROM b67_l) s JOIN LATERAL (SELECT s.x AS y) l ON true
SELECT count(*) FROM (SELECT a/b AS x FROM b67_l) s LEFT JOIN LATERAL (SELECT s.x AS y) l ON true
SELECT count(*) FROM (SELECT a/b AS x, a FROM b67_l) s, LATERAL (SELECT s.x FROM b67_l t WHERE t.a = s.a) l
SELECT l.y FROM (SELECT a/b AS x FROM b67_l) s, LATERAL (SELECT s.x AS y) l
SELECT count(*) FROM (SELECT a/b AS x FROM b67_l) s, LATERAL (SELECT s.x WHERE s.x > 0) l
SELECT count(*) FROM (SELECT a/b AS x FROM b67_l) s, LATERAL (SELECT s.x, nosuch) l
SELECT count(*) FROM (SELECT a FROM b67_l) s, LATERAL (SELECT s.a / 0 AS q) l
SELECT count(*) FROM (SELECT a FROM b67_l) s, LATERAL (SELECT s.a / 0 AS q, s.a AS r) l WHERE l.r > 1
SELECT count(*) FROM (SELECT a FROM b67_l) s, LATERAL (SELECT DISTINCT s.a / 0 AS q) l
SELECT count(*) FROM (SELECT a FROM b67_l) s, LATERAL (SELECT random() + s.a / 0 AS q) l
SELECT count(*) FROM (SELECT a/b AS x FROM b67_l) s, LATERAL (SELECT s.x LIMIT 1) l
SELECT count(*) FROM (SELECT a/b AS x FROM b67_l) s, LATERAL (SELECT s.x UNION ALL SELECT s.x) l
SELECT count(*) FROM (SELECT a/b AS x FROM b67_l) s, LATERAL (SELECT sum(s.x)) l
SELECT count(*) FROM (SELECT a/b AS x FROM b67_l) s, LATERAL (SELECT s.x FROM generate_series(1, 2)) l
