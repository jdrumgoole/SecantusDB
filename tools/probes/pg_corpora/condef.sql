SELECT conname, pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid = 'cdf_p'::regclass ORDER BY conname
SELECT conname, pg_get_constraintdef(oid), confmatchtype FROM pg_constraint WHERE conrelid = 'cdf_c'::regclass ORDER BY conname
INSERT INTO cdf_c VALUES (1, 1, NULL, 1, 1, NULL)
INSERT INTO cdf_c VALUES (2, NULL, NULL, 1, 1, NULL)
INSERT INTO cdf_c VALUES (3, 1, 2, 1, 1, 'x')
SELECT pg_get_constraintdef(0)
CREATE TABLE cdf_bad (a int REFERENCES cdf_p (u) MATCH PARTIAL)
