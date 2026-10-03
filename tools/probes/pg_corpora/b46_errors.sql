# reference-version: 15
# Batch 46: LIKE over a non-string column against an untyped literal is
# 42883 in the select list too (it answered NULL); a FROM-less subquery
# naming a sibling FROM item is 42P01 (it was 42703).
SELECT a LIKE 'x' FROM b46e
SELECT a NOT ILIKE 'x' FROM b46e
SELECT d LIKE 'x' FROM b46e
SELECT s LIKE 'x' FROM b46e
SELECT a::text LIKE '1' FROM b46e
SELECT * FROM b46e WHERE a LIKE 'x'
SELECT a FROM b46e x, (SELECT x.a) s
SELECT a FROM b46e x, (SELECT x.a + 1 AS b) s
SELECT 1 FROM b46e x, LATERAL (SELECT x.a) s
SELECT * FROM b46e x WHERE EXISTS (SELECT 1 FROM b46e y, (SELECT x.a) z)
