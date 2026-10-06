# reference-version: 15
# A subquery inside INSERT ... VALUES (batch 63; it was 0A000 "SubLink is
# not supported yet"). Every row's subquery reads the table as it was
# before the statement.
INSERT INTO b63_vs VALUES (1, (SELECT 5))
INSERT INTO b63_vs VALUES ((SELECT max(x) FROM b63_vs2), 3)
INSERT INTO b63_vs VALUES (2, (SELECT max(v) FROM b63_vs))
INSERT INTO b63_vs VALUES (3, 1 + (SELECT count(*) FROM b63_vs)), (4, (SELECT count(*) FROM b63_vs))
INSERT INTO b63_vs VALUES (5, (SELECT x FROM b63_vs2 WHERE x > 7))
INSERT INTO b63_vs VALUES (6, (SELECT x FROM b63_vs2))
INSERT INTO b63_vs VALUES (6, (SELECT x FROM b63_vs2 WHERE false))
INSERT INTO b63_vs VALUES (8, CASE WHEN EXISTS (SELECT 1 FROM b63_vs2) THEN 1 ELSE 0 END)
INSERT INTO b63_vs VALUES (10, (SELECT 1) + (SELECT 2)) RETURNING id, v
INSERT INTO b63_vs (id) VALUES ((SELECT max(id) + 1 FROM b63_vs))
SELECT * FROM b63_vs ORDER BY id
