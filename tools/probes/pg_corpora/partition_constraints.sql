# A partition's own column options and constraints hold for the rows it
# takes, written through the parent or the partition itself.
INSERT INTO pcx (id, v) VALUES (1, 5)
SELECT * FROM pcx ORDER BY id
INSERT INTO pcx (id, v) VALUES (2, NULL)
INSERT INTO pcx (id, v) VALUES (3, -1)
INSERT INTO pcx1 (id, v) VALUES (4, 4)
SELECT * FROM pcx1 ORDER BY id
SELECT column_name, is_nullable, column_default FROM information_schema.columns WHERE table_name = 'pcx1' ORDER BY ordinal_position
INSERT INTO pcx VALUES (11, 1, 'a'), (11, 2, 'b')
INSERT INTO pcx VALUES (11, 1, 'a')
INSERT INTO pcx2 VALUES (11, 1, 'a')
UPDATE pcx SET id = 11 WHERE id = 1
INSERT INTO pcx VALUES (21, 9, 'x')
INSERT INTO pcx VALUES (21, 1, 'x'), (22, 2, 'x')
SELECT count(*) FROM pcx
CREATE TABLE pcx4 PARTITION OF pcx (nosuch NOT NULL) FOR VALUES FROM (30) TO (40)
DROP TABLE pcx CASCADE
