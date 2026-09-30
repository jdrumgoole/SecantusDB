DROP TABLE IF EXISTS rc_t
CREATE TABLE rc_t (id int PRIMARY KEY, b bool, ts timestamp, s text)
INSERT INTO rc_t VALUES (1, true, '2021-01-01 00:00:00.123456', 'a1'), (2, false, '2021-06-01 12:00:00', 'b2'), (3, NULL, NULL, 'c3')
