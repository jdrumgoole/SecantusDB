DROP TABLE IF EXISTS b59_d
CREATE TABLE b59_d (id int PRIMARY KEY, n numeric, f float8, t text, b bool, a int[], j jsonb)
INSERT INTO b59_d VALUES (1, 1.0, 0.0, 'a', true, '{1,2}', '{"x":1}'), (2, 1, -0.0, 'A', true, '{1,2}', '{"x":1.0}'), (3, 1.00, 'NaN', 'b', false, '{2,1}', '[1]'), (4, NULL, 'NaN', NULL, NULL, NULL, NULL), (5, NULL, NULL, NULL, NULL, NULL, NULL), (6, 2.50, 'Infinity', 'b ', false, '{}', '{}'), (7, 2.5, '-Infinity', 'é', true, '{NULL}', 'null'), (8, 1e40, 1.5, 'e' || chr(769), true, '{NULL}', '"s"'), (9, 1.0e40, 1.5, 'a', false, '{1,2}', '{"x":1}')
INSERT INTO b59_d SELECT g, g % 37, (g % 19) / 4.0, 'v' || (g % 53), g % 2 = 0, ARRAY[g % 5], jsonb_build_object('k', g % 7) FROM generate_series(100, 3000) g
