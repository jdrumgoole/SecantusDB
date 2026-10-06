DROP TABLE IF EXISTS b62_ts
CREATE TABLE b62_ts (id int PRIMARY KEY, ts timestamp, tz timestamptz, k int)
INSERT INTO b62_ts SELECT g, timestamp '2024-01-01 00:00:00' + (g % 7) * interval '1 microsecond' + (g % 3) * interval '1 millisecond', timestamptz '2024-01-01 00:00:00+00' + (g % 5) * interval '250 microseconds', g % 4 FROM generate_series(1, 400) g
INSERT INTO b62_ts VALUES (1000, NULL, NULL, NULL), (1001, NULL, NULL, 1)
