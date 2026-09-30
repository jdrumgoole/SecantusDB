DROP TABLE IF EXISTS fn
CREATE TABLE fn (id int, f float8, r real, n numeric)
INSERT INTO fn VALUES (1, 'NaN', 'NaN', 'NaN'), (2, 'Infinity', 'Infinity', 1e30), (3, 1, 1, 1), (4, '-Infinity', '-Infinity', -5), (5, NULL, NULL, NULL)
