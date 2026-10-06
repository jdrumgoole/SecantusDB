DROP TABLE IF EXISTS b67_g CASCADE
DROP TABLE IF EXISTS b67_d CASCADE
CREATE TABLE b67_g (id int, k int, g int, f float8, n numeric, t text, j jsonb)
INSERT INTO b67_g SELECT i, i % 5, CASE WHEN i % 11 = 0 THEN NULL ELSE i % 3 END, i * 0.1, i * 1.5, 'v' || (i % 4), ('{"a": ' || (i % 2) || '}')::jsonb FROM generate_series(1, 400) i
CREATE TABLE b67_d (k int, name text)
INSERT INTO b67_d VALUES (0, 'zero'), (1, 'one'), (2, 'two'), (3, 'three')
