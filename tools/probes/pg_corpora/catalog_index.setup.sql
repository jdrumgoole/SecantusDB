DROP TABLE IF EXISTS ci_t
CREATE TABLE ci_t (id int PRIMARY KEY, a int, b text, c int UNIQUE)
CREATE INDEX ci_ab ON ci_t (a, b)
CREATE INDEX ci_expr ON ci_t (lower(b))
