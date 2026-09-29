DROP TABLE IF EXISTS cdf_c
DROP TABLE IF EXISTS cdf_p
CREATE TABLE cdf_p (a int, b int, u text UNIQUE, PRIMARY KEY (a, b))
CREATE TABLE cdf_c (id int PRIMARY KEY, a int, b int, n int CHECK (n > 0), m int, CONSTRAINT m_range CHECK (m >= 0 AND m < 100), u text REFERENCES cdf_p (u) ON DELETE CASCADE, FOREIGN KEY (a, b) REFERENCES cdf_p (a, b) MATCH FULL ON UPDATE SET NULL DEFERRABLE)
INSERT INTO cdf_p VALUES (1, 2, 'x')
