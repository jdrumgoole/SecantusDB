DROP TABLE IF EXISTS b59_g
CREATE TABLE b59_g (id int PRIMARY KEY, k int, t text, n numeric, f float8)
INSERT INTO b59_g SELECT g, CASE WHEN g % 9 = 0 THEN NULL ELSE g % 23 END, 'r' || (g % 7), ((g % 4) || '.' || repeat('0', g % 3))::numeric, CASE WHEN g % 50 = 0 THEN 'NaN'::float8 ELSE (g % 5) * 0.5 END FROM generate_series(1, 2000) g
