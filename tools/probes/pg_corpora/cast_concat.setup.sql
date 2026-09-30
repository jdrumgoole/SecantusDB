DROP TABLE IF EXISTS xcc_t
DROP TABLE IF EXISTS xcc_o
CREATE TABLE xcc_t (n numeric(10,2), m numeric, f float8, i int, d date, b bool, t timestamp, iv interval, u uuid, tz timestamptz, r real, s text)
INSERT INTO xcc_t VALUES (1.5, 2.25, 1e20, 3, '2020-01-01', true, '2020-01-01 10:00:00.5', '1 day 2 hours', 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', '2020-01-01 10:00:00.5+00', 2.25, 'abcdef')
CREATE TABLE xcc_o (a int, b int)
INSERT INTO xcc_o VALUES (1, 2), (2, 1)
