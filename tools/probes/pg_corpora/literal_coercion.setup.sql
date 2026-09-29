DROP TABLE IF EXISTS lt1
CREATE TABLE lt1 (id int PRIMARY KEY, n int, b bigint, x numeric, f float8, d date, t timestamptz, u timestamp, tm time, iv interval, ok bool, uu uuid, s text)
INSERT INTO lt1 VALUES (1, 5, 50, 1.5, 2.5, '2026-01-01', '2026-01-01 10:00:00+00', '2026-01-01 10:00:00', '10:00', '1 day', true, 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', 'x'), (2, 7, 70, 3.5, 4.5, '2026-06-01', '2026-06-01 10:00:00.5+00', '2026-06-01 10:00:00.25', '12:30:00.5', '2 days', false, 'b0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', 'y')
INSERT INTO lt1 (id, t, u) VALUES (3, '2026-06-01 10:00:00.123456+00', '2026-06-01 10:00:00.123456'), (4, '2026-06-01 10:00:00.123+00', '2026-06-01 10:00:00.123')
