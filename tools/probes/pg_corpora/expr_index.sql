CREATE UNIQUE INDEX ei_email ON ei_t (lower(email))
INSERT INTO ei_t VALUES (3, 'a@X.com', 5, 6)
INSERT INTO ei_t VALUES (4, 'c@x.com', 5, 6)
UPDATE ei_t SET email = 'B@X.COM' WHERE id = 4
UPDATE ei_t SET email = 'C@X.COM' WHERE id = 4
INSERT INTO ei_t VALUES (5, NULL, 1, 1), (6, NULL, 1, 1)
SELECT id, email FROM ei_t ORDER BY id
CREATE INDEX ON ei_t ((a + b))
CREATE INDEX ei_ab ON ei_t (a DESC NULLS LAST, b NULLS FIRST)
SELECT indexname, indexdef FROM pg_indexes WHERE tablename = 'ei_t' ORDER BY indexname
CREATE UNIQUE INDEX ei_sum ON ei_t ((a + b))
CREATE UNIQUE INDEX ei_part ON ei_t (lower(email)) WHERE a > 100
INSERT INTO ei_t VALUES (7, 'q@x.com', 200, 0), (8, 'Q@X.com', 1, 0)
INSERT INTO ei_t VALUES (9, 'Q@x.com', 300, 0)
SELECT count(*) FROM ei_t
DROP INDEX ei_email
INSERT INTO ei_t VALUES (10, 'a@x.com', 1, 1)
SELECT count(*) FROM ei_t WHERE lower(email) = 'a@x.com'
CREATE INDEX ON ei_t (nope(a))
CREATE INDEX ON ei_t ((zz + 1))
