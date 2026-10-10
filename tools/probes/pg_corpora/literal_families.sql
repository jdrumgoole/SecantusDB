# reference-version: 15
# Unprepared statements whose values change: after the second statement of a shape the server may reuse a plan learned for the shape (plan_cache::literal_family), and every answer must still be PostgreSQL's. Each shape runs twice before the values that matter.
SELECT t FROM lf_t WHERE k = 1
SELECT t FROM lf_t WHERE k = 2
SELECT t FROM lf_t WHERE k = 3
SELECT t FROM lf_t WHERE k = 99
SELECT t FROM lf_t WHERE k = -5
SELECT t FROM lf_t WHERE k = 100
SELECT t FROM lf_t WHERE k = 007
SELECT t FROM lf_t WHERE k = 2147483647
SELECT t FROM lf_t WHERE k = 2147483648
SELECT t FROM lf_t WHERE k = -2147483648
SELECT t FROM lf_t WHERE k = 9223372036854775807
SELECT t FROM lf_t WHERE k = 9223372036854775808
SELECT t FROM lf_t WHERE k = 5.0
SELECT t FROM lf_t WHERE k = 5e0
SELECT t FROM lf_t WHERE k = +5
SELECT t FROM lf_t WHERE k = '5'
SELECT t FROM lf_t WHERE k = 'x'
SELECT t FROM lf_t WHERE k = NULL
SELECT t FROM lf_t WHERE k = k
SELECT t FROM lf_t WHERE k = (5)
SELECT t FROM lf_t WHERE k = 5 /* c */
SELECT t FROM lf_t WHERE k =1
SELECT t FROM lf_t WHERE k =2
SELECT t FROM lf_t WHERE k =3
SELECT t FROM lf_t WHERE k =-5
SELECT t FROM lf_t WHERE k = -5
SELECT t FROM lf_t WHERE k !=1
SELECT t FROM lf_t WHERE k !=2
SELECT t FROM lf_t WHERE k !=3
SELECT t FROM lf_t WHERE k !=-5
SELECT t FROM lf_t WHERE k != -5
SELECT k FROM lf_t WHERE k < 1 ORDER BY k
SELECT k FROM lf_t WHERE k < 2 ORDER BY k
SELECT k FROM lf_t WHERE k < 3 ORDER BY k
SELECT k FROM lf_t WHERE k < -4 ORDER BY k
SELECT k FROM lf_t WHERE k < -5 ORDER BY k
SELECT k FROM lf_t WHERE k < -6 ORDER BY k
SELECT k FROM lf_t WHERE k - 1 = 0
SELECT k FROM lf_t WHERE k - 2 = 0
SELECT k FROM lf_t WHERE k - 3 = 0
SELECT k FROM lf_t WHERE k - -5 = 0
SELECT k FROM lf_t WHERE k IN (1, 3) ORDER BY k
SELECT k FROM lf_t WHERE k IN (2, 3) ORDER BY k
SELECT k FROM lf_t WHERE k IN (4, 3) ORDER BY k
SELECT k FROM lf_t WHERE k IN (-5, 3) ORDER BY k
SELECT k FROM lf_t WHERE k IN (3, 3) ORDER BY k
SELECT k FROM lf_t WHERE b = 1
SELECT k FROM lf_t WHERE b = 2
SELECT k FROM lf_t WHERE b = 3000000000
SELECT k FROM lf_t WHERE b = 3
SELECT k FROM lf_t WHERE b = -5000000000
SELECT k FROM lf_t WHERE b = 2147483648
SELECT k FROM lf_t WHERE b = 9223372036854775808
SELECT k FROM lf_t WHERE s = 1
SELECT k FROM lf_t WHERE s = 2
SELECT k FROM lf_t WHERE s = 3
SELECT k FROM lf_t WHERE s = 40000
SELECT k FROM lf_t WHERE s = -5
SELECT k FROM lf_t WHERE s = 3000000000
SELECT k FROM lf_t WHERE f = 1
SELECT k FROM lf_t WHERE f = 2
SELECT k FROM lf_t WHERE f = 0
SELECT k FROM lf_t WHERE f = 5
SELECT k FROM lf_t WHERE f = 5.5
SELECT k FROM lf_t WHERE n = 1
SELECT k FROM lf_t WHERE n = 2
SELECT k FROM lf_t WHERE n = 5
SELECT k FROM lf_t WHERE n = 0
SELECT k FROM lf_t WHERE t = 'v1'
SELECT k FROM lf_t WHERE t = 'v2'
SELECT k FROM lf_t WHERE t = 'v3'
SELECT k FROM lf_t WHERE t = 'V3'
SELECT k FROM lf_t WHERE t = 'v3 '
SELECT k FROM lf_t WHERE t = ''
SELECT k FROM lf_t WHERE t = 'it''s'
SELECT k FROM lf_t WHERE t = '5'
SELECT k FROM lf_t WHERE t = 5
SELECT k FROM lf_t WHERE t = 'neg'
SELECT k FROM lf_t WHERE t = E'v4'
SELECT k FROM lf_t WHERE t = 'v' 'x'
SELECT k FROM lf_t WHERE t = $$v5$$
SELECT k FROM lf_t WHERE t = NULL
SELECT k FROM lf_t WHERE vc = 'c1'
SELECT k FROM lf_t WHERE vc = 'c2'
SELECT k FROM lf_t WHERE vc = 'c3'
SELECT k FROM lf_t WHERE vc = 'c3 '
SELECT k FROM lf_t WHERE vc = ' sp '
SELECT k FROM lf_t WHERE vc = ' sp'
SELECT k FROM lf_t WHERE vc = '5'
SELECT k FROM lf_t WHERE vc = 'toolong'
SELECT k FROM lf_t WHERE c = 'h1'
SELECT k FROM lf_t WHERE c = 'h2'
SELECT k FROM lf_t WHERE c = 'h3'
SELECT k FROM lf_t WHERE c = 'h3 '
SELECT k FROM lf_t WHERE c = ' sp'
SELECT k FROM lf_t WHERE c = ' sp '
SELECT k FROM lf_t WHERE c = '5'
SELECT k FROM lf_t WHERE d = '2024-01-02'
SELECT k FROM lf_t WHERE d = '2024-01-03'
SELECT k FROM lf_t WHERE d = '2024-01-04'
SELECT k FROM lf_t WHERE d = 'Jan 5 2024'
SELECT k FROM lf_t WHERE d = 'nope'
SELECT k FROM lf_t WHERE x = 'x1'
SELECT k FROM lf_t WHERE x = 'x2'
SELECT k FROM lf_t WHERE x = 'x3'
SELECT k FROM lf_t WHERE x = '\x7833'
SELECT k FROM lf_t WHERE x = '\xZZ'
SELECT k FROM lf_t WHERE x > 'x1' ORDER BY k
SELECT k FROM lf_t WHERE x > 'x8' ORDER BY k
SELECT k FROM lf_t WHERE x > 'x9' ORDER BY k
SELECT k FROM lf_t WHERE k = 1 AND t = 'v3'
SELECT k FROM lf_t WHERE k = 2 AND t = 'v3'
SELECT k FROM lf_t WHERE k = 3 AND t = 'v3'
SELECT k FROM lf_t WHERE k = 4 AND t = 'v3'
SELECT k FROM lf_t WHERE k > 1 AND k < 6 OR NOT k <= 100 ORDER BY k
SELECT k FROM lf_t WHERE k > 2 AND k < 6 OR NOT k <= 100 ORDER BY k
SELECT k FROM lf_t WHERE k > 3 AND k < 6 OR NOT k <= 100 ORDER BY k
SELECT k FROM lf_t WHERE k > -5 AND k < 6 OR NOT k <= 100 ORDER BY k
SELECT k FROM lf_t WHERE k >= 1 ORDER BY k LIMIT 2
SELECT k FROM lf_t WHERE k >= 2 ORDER BY k LIMIT 2
SELECT k FROM lf_t WHERE k >= 3 ORDER BY k LIMIT 2
SELECT k FROM lf_t WHERE k >= -5 ORDER BY k LIMIT 2
SELECT k FROM lf_t WHERE k >= 3 ORDER BY k LIMIT 1
SELECT k FROM lf_t WHERE k >= 3 ORDER BY k LIMIT 2
SELECT k FROM lf_t WHERE k >= 3 ORDER BY k LIMIT 3
SELECT k FROM lf_t WHERE k >= 3 ORDER BY k LIMIT 0
SELECT k FROM lf_t WHERE k >= 3 ORDER BY k LIMIT 2 OFFSET 1
SELECT k FROM lf_t WHERE k >= 3 ORDER BY k LIMIT 2 OFFSET 2
SELECT k FROM lf_t WHERE k >= 3 ORDER BY k LIMIT 2 OFFSET 3
SELECT k, 1 FROM lf_t WHERE k = 3
SELECT k, 2 FROM lf_t WHERE k = 3
SELECT k, 3 FROM lf_t WHERE k = 3
SELECT k, 'a' FROM lf_t WHERE k = 3
SELECT k + 1 FROM lf_t WHERE k = 3
SELECT k + 2 FROM lf_t WHERE k = 3
SELECT k + 3 FROM lf_t WHERE k = 3
SELECT k FROM lf_t WHERE k = 4 % 3
SELECT k FROM lf_t WHERE k = 5 % 3
SELECT k FROM lf_t WHERE k = 6 % 3
SELECT k FROM lf_t WHERE k = 7 % 3
SELECT k FROM lf_t WHERE k BETWEEN 1 AND 4 ORDER BY k
SELECT k FROM lf_t WHERE k BETWEEN 2 AND 4 ORDER BY k
SELECT k FROM lf_t WHERE k BETWEEN 3 AND 4 ORDER BY k
SELECT k FROM lf_t WHERE t LIKE 'v1%' ORDER BY k
SELECT k FROM lf_t WHERE t LIKE 'v2%' ORDER BY k
SELECT k FROM lf_t WHERE t LIKE 'v_' ORDER BY k
SELECT count(*) FROM lf_t WHERE k > 1
SELECT count(*) FROM lf_t WHERE k > 2
SELECT count(*) FROM lf_t WHERE k > 3
SELECT count(*) FROM lf_t WHERE k > 100
SELECT "k" FROM lf_t WHERE "k" = 1
SELECT "k" FROM lf_t WHERE "k" = 2
SELECT "k" FROM lf_t WHERE "k" = 3
INSERT INTO lf_w VALUES (1, 'a', 1, 1, 'a', 1)
INSERT INTO lf_w VALUES (2, 'a', 1, 1, 'a', 1)
INSERT INTO lf_w VALUES (3, 'a', 1, 1, 'a', 1)
INSERT INTO lf_w VALUES (3, 'a', 1, 1, 'a', 1)
INSERT INTO lf_w VALUES (-4, 'a', 1, 1, 'a', 1)
INSERT INTO lf_w VALUES (2147483648, 'a', 1, 1, 'a', 1)
INSERT INTO lf_w VALUES ('9', 'a', 1, 1, 'a', 1)
INSERT INTO lf_w VALUES (NULL, 'a', 1, 1, 'a', 1)
INSERT INTO lf_w (k, t) VALUES (21, 'x')
INSERT INTO lf_w (k, t) VALUES (22, 'y')
INSERT INTO lf_w (k, t) VALUES (23, 'it''s')
INSERT INTO lf_w (k, t) VALUES (24, '')
INSERT INTO lf_w (k, t) VALUES (25, NULL)
INSERT INTO lf_w (k, t) VALUES (26, 5)
INSERT INTO lf_w (k, t, i) VALUES (31, 'a', 5)
INSERT INTO lf_w (k, t, i) VALUES (32, 'b', 6)
INSERT INTO lf_w (k, t, i) VALUES (33, 'c', 999)
INSERT INTO lf_w (k, t, i) VALUES (34, 'd', 1000)
INSERT INTO lf_w (k, t, i) VALUES (35, 'e', -7)
INSERT INTO lf_w (k, t, b) VALUES (41, 'a', 5)
INSERT INTO lf_w (k, t, b) VALUES (42, 'b', 6)
INSERT INTO lf_w (k, t, b) VALUES (43, 'c', 3000000000)
INSERT INTO lf_w (k, t, b) VALUES (44, 'd', -7)
INSERT INTO lf_w (k, t, vc) VALUES (51, 'a', 'ab')
INSERT INTO lf_w (k, t, vc) VALUES (52, 'b', 'cd')
INSERT INTO lf_w (k, t, vc) VALUES (53, 'c', 'toolong')
INSERT INTO lf_w (k, t, vc) VALUES (54, 'd', 'abcd')
INSERT INTO lf_w (k, t, s) VALUES (61, 'a', 5)
INSERT INTO lf_w (k, t, s) VALUES (62, 'b', 6)
INSERT INTO lf_w (k, t, s) VALUES (63, 'c', 40000)
INSERT INTO lf_w (k, t, s) VALUES (64, 'd', -7)
INSERT INTO lf_w (k, t) VALUES (71, 'a') RETURNING k, t
INSERT INTO lf_w (k, t) VALUES (72, 'b') RETURNING k, t
INSERT INTO lf_w (k, t) VALUES (73, 'c') RETURNING k, t
UPDATE lf_w SET t = 'p' WHERE k = 1
UPDATE lf_w SET t = 'q' WHERE k = 1
UPDATE lf_w SET t = 'r' WHERE k = 1
UPDATE lf_w SET t = '' WHERE k = 1
UPDATE lf_w SET t = NULL WHERE k = 1
UPDATE lf_w SET t = 5 WHERE k = 1
UPDATE lf_w SET i = 5 WHERE k = 2
UPDATE lf_w SET i = 6 WHERE k = 2
UPDATE lf_w SET i = 7 WHERE k = 2
UPDATE lf_w SET i = 1000 WHERE k = 2
UPDATE lf_w SET i = -3 WHERE k = 2
UPDATE lf_w SET i = 3000000000 WHERE k = 2
UPDATE lf_w SET i = i + 1 WHERE k = 3
UPDATE lf_w SET i = i + 2 WHERE k = 3
UPDATE lf_w SET i = i + 3 WHERE k = 3
UPDATE lf_w SET i = i + 2000 WHERE k = 3
UPDATE lf_w SET vc = 'a' WHERE k = 2
UPDATE lf_w SET vc = 'b' WHERE k = 2
UPDATE lf_w SET vc = 'c' WHERE k = 2
UPDATE lf_w SET vc = 'toolong' WHERE k = 2
UPDATE lf_w SET t = 'z' WHERE k = 41
UPDATE lf_w SET t = 'z' WHERE k = 42
UPDATE lf_w SET t = 'z' WHERE k = 43
UPDATE lf_w SET t = 'z' WHERE k = 999
DELETE FROM lf_w WHERE k = 61
DELETE FROM lf_w WHERE k = 62
DELETE FROM lf_w WHERE k = 64
DELETE FROM lf_w WHERE k = 999
DELETE FROM lf_w WHERE t = 'x'
DELETE FROM lf_w WHERE t = 'y'
DELETE FROM lf_w WHERE t = 'it''s'
SELECT k, t, i, b, vc, s FROM lf_w WHERE k > -100 ORDER BY k
SELECT k, t, i, b, vc, s FROM lf_w WHERE k > -99 ORDER BY k
SELECT k, t, i, b, vc, s FROM lf_w WHERE k > -98 ORDER BY k
# A catalog or role change between two statements of one shape is seen.
SELECT t FROM lf_t WHERE k = 4
ALTER TABLE lf_t RENAME COLUMN t TO tt
SELECT t FROM lf_t WHERE k = 5
SELECT tt FROM lf_t WHERE k = 5
ALTER TABLE lf_t RENAME COLUMN tt TO t
SELECT t FROM lf_t WHERE k = 6
CREATE INDEX lf_s ON lf_t (s)
SELECT k FROM lf_t WHERE s = 9
SELECT k FROM lf_t WHERE s = 10
DROP ROLE IF EXISTS lf_r
CREATE ROLE lf_r
GRANT SELECT ON lf_t TO lf_r
ALTER TABLE lf_t ENABLE ROW LEVEL SECURITY
CREATE POLICY lf_p ON lf_t FOR SELECT TO lf_r USING (k < 4)
SELECT k FROM lf_t WHERE k >= 2 ORDER BY k
SET ROLE lf_r
SELECT k FROM lf_t WHERE k >= 2 ORDER BY k
SELECT k FROM lf_t WHERE k >= 3 ORDER BY k
SELECT k FROM lf_t WHERE k >= 1 ORDER BY k
RESET ROLE
SELECT k FROM lf_t WHERE k >= 11 ORDER BY k
ALTER TABLE lf_t DISABLE ROW LEVEL SECURITY
DROP POLICY lf_p ON lf_t
REVOKE SELECT ON lf_t FROM lf_r
DROP ROLE lf_r
SELECT 5
SELECT 6
SELECT 7
SELECT 'a'
SELECT 'b'
SELECT 'c'
