DROP TABLE IF EXISTS b45o
CREATE TABLE b45o (a int, b text, d date, ts timestamp, j jsonb, arr int[], f float8, bo bool, n numeric)
INSERT INTO b45o VALUES (1, 'x', '2020-01-01', '2020-01-01 10:00', '{"k":1}', '{1,2}', 1.5, true, 2.5)
