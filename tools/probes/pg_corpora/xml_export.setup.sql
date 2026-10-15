DROP TABLE IF EXISTS xmlexp_t
CREATE TABLE xmlexp_t (id int, "xmlfoo" text, "_xy" text, "a:b" bool, n float8, ts timestamp, tz timestamptz, b bytea, arr int[], c char(3))
INSERT INTO xmlexp_t values (1, 'a&b<c>"''', 'x', true, 1e20, '2020-01-02 03:04:05', '2020-01-02 03:04:05+00', '\x0102', array[1,null,3], 'ab'), (2, null, null, false, null, null, null, null, null, null)
