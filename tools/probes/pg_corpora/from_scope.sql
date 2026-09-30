SELECT c.name, x.total FROM sv_c c, LATERAL (SELECT sum(amt) total FROM sv_o WHERE cid = c.id) x ORDER BY c.id
SELECT c.name, x.total FROM sv_c c CROSS JOIN LATERAL (SELECT sum(amt) total FROM sv_o WHERE cid = c.id) x ORDER BY c.id
SELECT c.name, x.n FROM sv_c c JOIN LATERAL (SELECT count(*) n FROM sv_o o WHERE o.cid = c.id) x ON true ORDER BY c.id
SELECT c.name, x.amt FROM sv_c c LEFT JOIN LATERAL (SELECT amt FROM sv_o WHERE cid = c.id ORDER BY amt DESC LIMIT 1) x ON true ORDER BY c.id
SELECT c.name, (SELECT sum(amt) FROM sv_o WHERE cid = c.id) FROM sv_c c ORDER BY c.id
SELECT c.name, x.id FROM sv_c c, (SELECT id FROM sv_o WHERE id = 1) x ORDER BY c.id
SELECT c.name, x.total FROM sv_c c, (SELECT sum(amt) total FROM sv_o WHERE cid = c.id) x ORDER BY c.id
SELECT c.name, x.total FROM sv_c c JOIN (SELECT sum(amt) total FROM sv_o WHERE cid = c.id) x ON true ORDER BY c.id
SELECT * FROM (SELECT id FROM sv_o WHERE cid = sv_c.id) x
SELECT sv_o.id FROM sv_o o
SELECT o.id FROM sv_o o ORDER BY o.id
SELECT sv_o.id FROM sv_o ORDER BY sv_o.id
SELECT public.sv_o.id FROM sv_o ORDER BY 1
SELECT x.id FROM sv_o ORDER BY 1
SELECT count(*) FROM sv_o o WHERE o.amt > 1 GROUP BY o.cid ORDER BY o.cid
SELECT s.a FROM (SELECT 1 AS a) s
SELECT g.x FROM generate_series(1, 2) g(x)
SELECT o.id FROM sv_o o WHERE EXISTS (SELECT 1 FROM sv_c c WHERE c.id = o.cid) ORDER BY 1
SELECT o.id, (SELECT c.name FROM sv_c c WHERE c.id = o.cid) FROM sv_o o ORDER BY 1
SELECT c.relname FROM pg_class c WHERE c.relname IN ('sv_c', 'sv_o') ORDER BY 1
SELECT c.* FROM sv_c c ORDER BY 1
SELECT sv_c.* FROM sv_c ORDER BY 1
SELECT c.*, 1 AS one FROM sv_c c ORDER BY 1
SELECT row_to_json(c.*) FROM sv_c c ORDER BY c.id
