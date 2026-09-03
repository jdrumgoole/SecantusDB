DROP TABLE IF EXISTS dt9
CREATE TABLE dt9 (id int PRIMARY KEY, d date, ts timestamp, tz timestamptz, iv interval, tm time)
INSERT INTO dt9 VALUES (1,'2020-02-29','2020-02-29 13:45:56.789','2020-02-29 13:45:56.789+00','1 year 2 mons 3 days 04:05:06','13:45:56.789')
