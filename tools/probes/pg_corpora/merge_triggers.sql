# reference-version: 15
# MERGE fires each action type's statement triggers once: BEFORE for every
# type it names (INSERT, UPDATE, DELETE) up front, AFTER in reverse at the end,
# whether or not a row took that action.
DELETE FROM mlog
MERGE INTO mt USING ms ON mt.id = ms.id WHEN MATCHED AND ms.v = 10 THEN UPDATE SET v = ms.v WHEN MATCHED THEN DELETE WHEN NOT MATCHED THEN INSERT VALUES (ms.id, ms.v)
SELECT what FROM mlog ORDER BY n
SELECT * FROM mt ORDER BY id
DELETE FROM mlog
MERGE INTO mt USING (SELECT 99 AS id, 1 AS v) s ON mt.id = s.id WHEN MATCHED THEN UPDATE SET v = 0
SELECT what FROM mlog ORDER BY n
DELETE FROM mlog
MERGE INTO mt USING ms ON mt.id = ms.id WHEN MATCHED THEN DO NOTHING
SELECT what FROM mlog ORDER BY n
DELETE FROM mlog
MERGE INTO mt USING ms ON mt.id = ms.id WHEN NOT MATCHED THEN INSERT VALUES (ms.id + 100, ms.v) WHEN MATCHED THEN UPDATE SET v = mt.v + 1
SELECT what FROM mlog ORDER BY n
SELECT * FROM mt ORDER BY id
DROP TABLE IF EXISTS mn
DROP TABLE IF EXISTS ms2
CREATE TABLE mn (a int, b int)
INSERT INTO mn VALUES (1, 1), (1, 1), (2, 2), (3, NULL), (3, NULL)
CREATE TABLE ms2 (a int, b int)
INSERT INTO ms2 VALUES (1, 5), (3, 7)
MERGE INTO mn USING ms2 ON mn.a = ms2.a WHEN MATCHED THEN UPDATE SET b = ms2.b
SELECT * FROM mn ORDER BY a, b
INSERT INTO ms2 VALUES (2, 9), (2, 10)
MERGE INTO mn USING ms2 ON mn.a = ms2.a WHEN MATCHED AND mn.a = 2 THEN DELETE
MERGE INTO mn USING ms2 ON mn.a = ms2.a AND ms2.b < 10 WHEN MATCHED AND mn.a = 3 THEN DELETE WHEN MATCHED THEN DO NOTHING
SELECT * FROM mn ORDER BY a, b
