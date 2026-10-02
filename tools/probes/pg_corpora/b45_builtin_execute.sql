# reference-version: 15
# Batch 45: EXECUTE on a BUILT-IN is checked at the call, by the overload
# the call resolves to (abs(int4) refused, abs(numeric) / abs(int8) not).
REVOKE EXECUTE ON FUNCTION abs(int) FROM PUBLIC
SET ROLE b45r
SELECT abs(-1)
SELECT abs(-1.5)
SELECT abs(-1::bigint)
SELECT 1 WHERE abs(-2) = 2
SELECT abs(i) FROM b45bt
SELECT abs(n) FROM b45bt
SELECT t FROM b45bt WHERE abs(i) = 1
SELECT (SELECT abs(i) FROM b45bt)
SELECT length(t) FROM b45bt
UPDATE b45bt SET t = 'c' WHERE abs(i) = 1
DELETE FROM b45bt WHERE abs(i) = 7
RESET ROLE
SELECT abs(-1)
GRANT EXECUTE ON FUNCTION abs(int) TO b45r
SET ROLE b45r
SELECT abs(i) FROM b45bt
RESET ROLE
REVOKE EXECUTE ON FUNCTION abs(int) FROM b45r
GRANT EXECUTE ON FUNCTION abs(int) TO PUBLIC
SET ROLE b45r
SELECT abs(i) FROM b45bt
RESET ROLE
REVOKE EXECUTE ON FUNCTION abs(int) FROM PUBLIC
GRANT INSERT ON b45bt TO b45r
SET ROLE b45r
SELECT i FROM b45bt ORDER BY abs(i)
SELECT count(*) FROM b45bt GROUP BY abs(i)
SELECT count(*) FROM b45bt HAVING abs(count(*)::int) = 1
UPDATE b45bt SET i = abs(i)
INSERT INTO b45bt VALUES (abs(-3))
INSERT INTO b45bt SELECT abs(i) FROM b45bt
VALUES (abs(-1))
DELETE FROM b45bt WHERE i = 9 RETURNING abs(i)
RESET ROLE
GRANT EXECUTE ON FUNCTION abs(int) TO PUBLIC
SELECT i FROM b45bt ORDER BY i
