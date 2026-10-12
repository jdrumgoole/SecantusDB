DROP TABLE IF EXISTS bc_t
CREATE TABLE bc_t (id int PRIMARY KEY, b bytea, s text, n int, m macaddr, r int4range)
INSERT INTO bc_t VALUES (1, '\x0061626300ff', 'Hello World', 3, '08:00:2b:01:02:03', '[1,5)'), (2, '\x', '', 0, 'ff:ff:ff:ff:ff:ff', 'empty'), (3, NULL, NULL, NULL, NULL, NULL), (4, '\xc3a9e282ac', 'a.b.c', -2, '00:00:00:00:00:00', '[3,)')
DROP TABLE IF EXISTS bc_a
CREATE TABLE bc_a (id int PRIMARY KEY, iv interval, b bit(3), by bytea, a int[], n int, s2 int2, g text)
INSERT INTO bc_a VALUES (1, '1 day', B'101', '\x61', '{1,2}', 5, 3, 'x'), (2, '2 days 3 hours', B'011', '\x62', '{3,4}', 6, 1, 'x'), (3, NULL, NULL, NULL, NULL, NULL, NULL, 'y'), (4, '1 mon -1 day 00:00:01.5', B'110', '\x00', '{5,6}', 12, -7, 'y')
