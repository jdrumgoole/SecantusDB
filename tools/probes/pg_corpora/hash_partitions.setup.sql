DROP TABLE IF EXISTS hp_i CASCADE
DROP TABLE IF EXISTS hp_t CASCADE
DROP TABLE IF EXISTS hp_m CASCADE
DROP TABLE IF EXISTS hp_d CASCADE
CREATE TABLE hp_i (id int, v text) PARTITION BY HASH (id)
CREATE TABLE hp_i0 PARTITION OF hp_i FOR VALUES WITH (modulus 4, remainder 0)
CREATE TABLE hp_i1 PARTITION OF hp_i FOR VALUES WITH (modulus 4, remainder 1)
CREATE TABLE hp_i2 PARTITION OF hp_i FOR VALUES WITH (modulus 4, remainder 2)
CREATE TABLE hp_i3 PARTITION OF hp_i FOR VALUES WITH (modulus 4, remainder 3)
INSERT INTO hp_i SELECT g, 'v' || g FROM generate_series(-20, 40) g
INSERT INTO hp_i VALUES (NULL, 'null'), (2147483647, 'max'), (-2147483648, 'min')
CREATE TABLE hp_t (k text) PARTITION BY HASH (k)
CREATE TABLE hp_t0 PARTITION OF hp_t FOR VALUES WITH (modulus 3, remainder 0)
CREATE TABLE hp_t1 PARTITION OF hp_t FOR VALUES WITH (modulus 3, remainder 1)
CREATE TABLE hp_t2 PARTITION OF hp_t FOR VALUES WITH (modulus 3, remainder 2)
INSERT INTO hp_t VALUES (''), ('a'), ('ab'), ('abc'), ('abcd'), ('hello world'), ('twelve bytes'), ('thirteen bytes!'), ('a much longer string that spans several twelve byte blocks'), ('ünïcödé')
CREATE TABLE hp_m (a int8, b text, c date, u uuid) PARTITION BY HASH (a, b, c, u)
CREATE TABLE hp_m0 PARTITION OF hp_m FOR VALUES WITH (modulus 2, remainder 0)
CREATE TABLE hp_m1 PARTITION OF hp_m FOR VALUES WITH (modulus 4, remainder 1)
CREATE TABLE hp_m3 PARTITION OF hp_m FOR VALUES WITH (modulus 4, remainder 3)
INSERT INTO hp_m SELECT g * 1000000007, 'x' || g, date '2020-01-01' + g, ('a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a' || lpad(g::text, 2, '0'))::uuid FROM generate_series(1, 30) g
INSERT INTO hp_m VALUES (NULL, NULL, NULL, NULL), (-5, 'neg', '1999-12-31', NULL)
