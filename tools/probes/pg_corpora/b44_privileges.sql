# reference-version: 15
# Batch 44: a SERIAL default's nextval is checked for the inserting role (an
# identity column's is not); lastval() checks the sequence it came from;
# EXECUTE on the trigger function at CREATE TRIGGER; a built-in function's
# grant is per signature.
SET ROLE b44r
INSERT INTO b44sp (v) VALUES (1)
INSERT INTO b44sp (id, v) VALUES (5, 1)
INSERT INTO b44ip (v) VALUES (1)
SELECT lastval()
CREATE TRIGGER b44t BEFORE INSERT ON b44sp FOR EACH ROW EXECUTE FUNCTION b44tf()
RESET ROLE
GRANT USAGE ON SEQUENCE b44sp_id_seq TO b44r
GRANT SELECT ON SEQUENCE b44ip_id_seq TO b44r
GRANT EXECUTE ON FUNCTION b44tf() TO b44r
SET ROLE b44r
INSERT INTO b44sp (v) VALUES (2)
INSERT INTO b44ip (v) VALUES (2)
SELECT lastval()
CREATE TRIGGER b44t BEFORE INSERT ON b44sp FOR EACH ROW EXECUTE FUNCTION b44tf()
RESET ROLE
SELECT id, v FROM b44sp ORDER BY id
SELECT id, v FROM b44ip ORDER BY id
REVOKE EXECUTE ON FUNCTION abs(int) FROM PUBLIC
SELECT has_function_privilege('b44r', 'abs(int)', 'EXECUTE'), has_function_privilege('b44r', 'abs(numeric)', 'EXECUTE')
GRANT EXECUTE ON FUNCTION abs(int) TO PUBLIC
SELECT has_function_privilege('b44r', 'abs(int)', 'EXECUTE')
