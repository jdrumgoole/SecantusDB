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
