SELECT * FROM sv_c JOIN sv_o ON sv_o.cid = sv_c.id ORDER BY sv_o.id
SELECT c.*, o.amt FROM sv_c c LEFT JOIN sv_o o ON o.cid = c.id ORDER BY c.id, o.id
WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 5) SELECT sum(n) FROM r
SELECT DISTINCT ON (cid) cid, amt FROM sv_o ORDER BY cid, amt DESC
SELECT cid, array_agg(amt ORDER BY amt DESC), sum(amt) FILTER (WHERE amt > 6) FROM sv_o GROUP BY cid ORDER BY cid
SELECT id FROM sv_o ORDER BY id FETCH FIRST 2 ROWS ONLY
SELECT id FROM sv_o WHERE (cid, amt) IN ((1, 20), (2, 5)) ORDER BY id
SELECT id FROM sv_o FOR UPDATE SKIP LOCKED
SELECT date_trunc('month', at)::text, count(*) FROM sv_o GROUP BY 1 ORDER BY 1
SELECT meta->>'k', meta ? 'k', tags && ARRAY['a'] FROM sv_o ORDER BY id
INSERT INTO sv_c VALUES (1,'ann2','x') ON CONFLICT (id) DO UPDATE SET name = excluded.name RETURNING *
CREATE DOMAIN sv_pos AS int CHECK (VALUE > 0)
CREATE TABLE sv_m (id int PRIMARY KEY, q sv_pos)
INSERT INTO sv_m VALUES (1, -1)
CREATE TABLE sv_p (id int, region text) PARTITION BY LIST (region)
CREATE MATERIALIZED VIEW sv_mv AS SELECT cid, sum(amt) s FROM sv_o GROUP BY cid
SELECT * FROM sv_mv ORDER BY cid
REFRESH MATERIALIZED VIEW sv_mv
SELECT id FROM sv_o TABLESAMPLE SYSTEM (100) ORDER BY id
COMMENT ON TABLE sv_c IS 'customers'
SELECT obj_description('sv_c'::regclass)
GRANT SELECT ON sv_c TO PUBLIC
SELECT count(*) FROM sv_o o WHERE amt > (SELECT avg(amt) FROM sv_o i WHERE i.cid = o.cid)
SELECT id, lag(amt) OVER (PARTITION BY cid ORDER BY at), sum(amt) OVER w FROM sv_o WINDOW w AS (ORDER BY id) ORDER BY id
SELECT c.name, x.total FROM sv_c c, LATERAL (SELECT sum(amt) total FROM sv_o WHERE cid = c.id) x ORDER BY c.id
SELECT jsonb_agg(jsonb_build_object('id', id, 'amt', amt) ORDER BY id) FROM sv_o
SELECT string_agg(name, ',' ORDER BY name) FROM sv_c
SELECT to_char(at, 'YYYY-MM-DD HH24:MI'), extract(epoch from at)::bigint FROM sv_o ORDER BY id
SELECT id FROM sv_o WHERE at BETWEEN '2024-01-01' AND '2024-01-31' ORDER BY id
SELECT unnest(tags), id FROM sv_o ORDER BY 2, 1
SELECT regexp_matches('a1b22', '\d+', 'g')
SELECT coalesce(tier, 'none'), count(*) FROM sv_c GROUP BY 1 ORDER BY 1
SELECT * FROM (VALUES (1, 'x'), (2, 'y')) v(a, b) ORDER BY a
SELECT id FROM sv_o WHERE meta @> '{"k": 1}'
CREATE TEMP TABLE sv_tmp AS SELECT id, amt FROM sv_o WHERE amt > 6
SELECT count(*) FROM sv_tmp
TRUNCATE sv_tmp
UPDATE sv_o o SET amt = amt * 2 FROM sv_c c WHERE c.id = o.cid AND c.tier = 'silver' RETURNING o.id, o.amt
SELECT pg_size_pretty(pg_total_relation_size('sv_o')) IS NOT NULL
SELECT id FROM sv_o ORDER BY amt DESC NULLS LAST LIMIT 1
SELECT gen_random_uuid() IS NOT NULL, md5('x'), now() > '2020-01-01'
