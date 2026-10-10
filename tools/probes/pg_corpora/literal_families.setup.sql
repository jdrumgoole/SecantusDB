DROP TABLE IF EXISTS lf_t, lf_w CASCADE
CREATE TABLE lf_t (k int primary key, b bigint, s smallint, t text, vc varchar(5), c char(5), f float8, n numeric(10,2), d date, x bytea)
INSERT INTO lf_t SELECT i, i::bigint * 1000000000, i, 'v' || i, 'c' || i, 'h' || i, i + 0.5, i + 0.25, date '2024-01-01' + i, ('x' || i)::bytea FROM generate_series(0, 11) i
INSERT INTO lf_t VALUES (-5, -5000000000, -5, 'neg', 'n', 'n', -5.5, -5.25, '2023-12-31', 'n'), (100, 2147483648, 100, 'it''s', ' sp ', ' sp ', 0, 0, NULL, NULL), (102, 3, 102, '5', '5', '5', 5, 5, NULL, NULL)
CREATE TABLE lf_w (k int primary key, t text NOT NULL, i int CHECK (i < 1000), b bigint, vc varchar(4), s smallint)
