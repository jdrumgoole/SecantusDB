# reference-version: 15
# RETURNING * over UPDATE ... FROM / DELETE ... USING is the target's columns then each FROM item's; t.* is one relation's (RP-034).
UPDATE rs_t SET v = 1 WHERE k = 1 RETURNING rs_t.*
UPDATE rs_t x SET v = 2 WHERE k = 1 RETURNING x.*, 9
UPDATE rs_t SET v = 3 FROM rs_s s2 WHERE s2.k = rs_t.k AND s2.n = 100 RETURNING *
UPDATE rs_t SET v = 4 FROM rs_s s2 WHERE s2.k = rs_t.k AND s2.n = 100 RETURNING s2.*
UPDATE rs_t SET v = 5 FROM rs_s s2 WHERE s2.k = rs_t.k AND s2.n = 100 RETURNING rs_t.*, s2.n
UPDATE rs_t SET v = 6 FROM (SELECT 1 AS k, 'x' AS w) q WHERE q.k = rs_t.k RETURNING *
UPDATE rs_t SET v = 7 FROM rs_s s2 WHERE s2.k = rs_t.k AND s2.n = 100 RETURNING nope.*
DELETE FROM rs_t USING rs_s s2 WHERE s2.k = rs_t.k AND s2.n = 200 RETURNING rs_t.*
DELETE FROM rs_t USING rs_s s2 WHERE s2.k = rs_t.k AND s2.n = 100 RETURNING *
DELETE FROM rs_t WHERE k = 3 RETURNING rs_t.*
SELECT * FROM rs_t ORDER BY k
