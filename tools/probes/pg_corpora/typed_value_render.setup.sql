DROP TABLE IF EXISTS tvr_t CASCADE
CREATE TABLE tvr_t (k int primary key, l pg_lsn, m money, f float4, x xid8, i inet, r regclass, iv interval, p point, b bytea, t tid)
INSERT INTO tvr_t VALUES (1, '0/16', '1.5', 1.1, '5', '10.0.0.1/32', 'pg_class', '1 day 2 hours', '(1,2)', '\x01ff', '(0,1)'), (2, '1/0', '-2', 2.5, '7', '192.168.0.0/16', 'pg_type', '3 days', '(3,4)', '\x02', '(4,5)'), (3, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)
