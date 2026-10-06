DROP TABLE IF EXISTS b64_s CASCADE
CREATE TABLE b64_s (id int PRIMARY KEY, k int, g int, d float8, n numeric, t text, v tsvector, q tsquery)
CREATE INDEX b64_s_k ON b64_s (k)
CREATE INDEX b64_s_t ON b64_s (t)
INSERT INTO b64_s SELECT i, i % 10, i % 3, i / 7.0, i * 1.5, 'w' || (i % 4), to_tsvector('simple', 'a b' || (i % 3)), to_tsquery('simple', 'a & b' || (i % 2)) FROM generate_series(1, 200) i
