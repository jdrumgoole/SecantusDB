# ADD COLUMN with inline constraints, and row-valued IN / NOT IN over a
# subquery -- against PostgreSQL 14.
ALTER TABLE ac ADD COLUMN u int UNIQUE
ALTER TABLE ac ADD COLUMN c int CHECK (c > 0)
ALTER TABLE ac ADD COLUMN p int PRIMARY KEY
ALTER TABLE ac ADD COLUMN r int REFERENCES ac (id)
ALTER TABLE ac2 ADD COLUMN r int REFERENCES ac2 (id), ADD COLUMN q text UNIQUE
SELECT conname, pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid = 'ac2'::regclass ORDER BY 1
INSERT INTO ac2 VALUES (2, 9, 'x')
INSERT INTO ac2 VALUES (2, 1, 'x')
INSERT INTO ac2 VALUES (3, 1, 'x')
SELECT a, b FROM rc WHERE (a, b) IN (SELECT 1, 2 UNION ALL SELECT 2, 5) ORDER BY 1, 2
SELECT a, b FROM rc WHERE (a, b) NOT IN (SELECT 1, 2) ORDER BY 1, 2
SELECT (1, NULL) IN (SELECT 1, 2), (1, 2) NOT IN (SELECT 1, NULL::int), (1,2) IN (SELECT 1, 2 WHERE false)
SELECT a FROM rc WHERE (a, b) IN (SELECT 1)
