DROP TABLE IF EXISTS ep15_t;
DROP TYPE IF EXISTS ep15_c;
CREATE TYPE ep15_c AS (a int, b text);
CREATE TABLE ep15_t (id int, c ep15_c, x text);
