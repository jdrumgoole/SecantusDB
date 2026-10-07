DROP TABLE IF EXISTS b68_w CASCADE
DROP TABLE IF EXISTS b68_s CASCADE
CREATE TABLE b68_w (id int PRIMARY KEY, k int, g int, n numeric, b bool, j jsonb, pad text)
INSERT INTO b68_w SELECT i, i % 9, CASE WHEN i % 13 = 0 THEN NULL ELSE i % 4 END, i * 1.25, i % 5 = 0, ('{"a": ' || (i % 3) || '}')::jsonb, repeat(md5(i::text), 6) FROM generate_series(1, 1500) i
INSERT INTO b68_w VALUES (1501, NULL, NULL, NULL, NULL, NULL, NULL), (1502, 2, 1, 1.0, true, '{"a": 1.0}', 'zzz')
CREATE TABLE b68_s (id int, name text)
INSERT INTO b68_s SELECT i, 'n' || i FROM generate_series(0, 6) i
INSERT INTO b68_s VALUES (NULL, 'nul'), (3, 'three-again')
