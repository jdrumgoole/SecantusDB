# A column whose name holds a dot or a leading $ is ONE top-level field, never
# an MQL path or operator: every read and write must address the literal key.
SELECT * FROM b36_d ORDER BY id
SELECT id FROM b36_d WHERE "dot.s" = 'p' ORDER BY id
SELECT id FROM b36_d WHERE "dot.s" IN ('q') ORDER BY id
SELECT id FROM b36_d WHERE "$x" > 15 ORDER BY id
SELECT id FROM b36_d WHERE "a.b.c" = 200
SELECT id FROM b36_d WHERE "sp ace" = 's3'
SELECT id FROM b36_d WHERE "dot.s" IS NULL
SELECT id FROM b36_d WHERE "dot.s" LIKE 'p%' ORDER BY id
UPDATE b36_d SET "dot.s" = 'x' WHERE id = 1
SELECT id, "dot.s" FROM b36_d ORDER BY id
UPDATE b36_d SET "$x" = "$x" + 1 WHERE "dot.s" = 'q'
SELECT id, "$x" FROM b36_d ORDER BY id
UPDATE b36_d SET "a.b.c" = NULL WHERE "$x" = 30
SELECT id, "a.b.c" FROM b36_d ORDER BY id
UPDATE b36_d SET "sp ace" = 'z' WHERE "sp ace" = 's2' RETURNING id, "sp ace", "dot.s"
SELECT "dot.s", "$x" FROM b36_d ORDER BY "dot.s" DESC, "$x"
SELECT "dot.s", count(*), sum("$x") FROM b36_d GROUP BY "dot.s" ORDER BY "dot.s"
SELECT DISTINCT "dot.s" FROM b36_d ORDER BY 1
SELECT max("$x"), min("a.b.c") FROM b36_d
CREATE INDEX b36_d_ix ON b36_d ("dot.s")
SELECT id FROM b36_d WHERE "dot.s" = 'p'
SELECT id FROM b36_d WHERE "dot.s" = 'x'
INSERT INTO b36_e VALUES (3, 'p', 9)
INSERT INTO b36_e VALUES (3, 'r', 9) ON CONFLICT ("dot.s") DO UPDATE SET "$y" = excluded."$y"
INSERT INTO b36_e VALUES (4, 'p', 99) ON CONFLICT ("dot.s") DO UPDATE SET "$y" = excluded."$y", "dot.s" = 'pp'
INSERT INTO b36_e VALUES (5, 'q', 1) ON CONFLICT ("dot.s") DO NOTHING
SELECT * FROM b36_e ORDER BY k
SELECT d.id, e.k FROM b36_d d JOIN b36_e e ON d."dot.s" = e."dot.s" ORDER BY 1, 2
SELECT d.id, e."$y" FROM b36_d d LEFT JOIN b36_e e ON e."$y" = d.id ORDER BY 1
DELETE FROM b36_d WHERE "dot.s" = 'q' RETURNING id, "dot.s"
DELETE FROM b36_e WHERE "$y" = 99
SELECT id, "dot.s" FROM b36_d ORDER BY id
SELECT k, "dot.s" FROM b36_e ORDER BY k
SELECT id FROM b36_d WHERE "dot.s" = 'x' AND "$x" = 10
SELECT id FROM b36_d WHERE "dot.s" = 'x' OR "$x" = 30 ORDER BY id
CREATE UNIQUE INDEX b36_d_ux ON b36_d ("$x")
INSERT INTO b36_d VALUES (9, 'n', 10, 's9', 900)
UPDATE b36_d SET "$x" = 30 WHERE id = 1
UPDATE b36_e SET "dot.s" = 'r' WHERE k = 2
INSERT INTO b36_d VALUES (8, 'x', 80, 's8', 800) ON CONFLICT ("$x") DO UPDATE SET "a.b.c" = excluded."a.b.c"
INSERT INTO b36_d VALUES (7, 'y', 80, 's7', 700) ON CONFLICT ("$x") DO UPDATE SET "a.b.c" = excluded."a.b.c"
SELECT id, "dot.s", "$x", "a.b.c" FROM b36_d ORDER BY id
UPDATE b36_d SET "dot.s" = NULL, "$x" = NULL WHERE id = 8
SELECT id FROM b36_d WHERE "dot.s" IS NULL AND "$x" IS NULL
DELETE FROM b36_d WHERE "$x" IS NULL
SELECT count(*) FROM b36_d
DROP TABLE b36_d
DROP TABLE b36_e
