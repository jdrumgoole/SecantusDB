CREATE TRIGGER t_upper BEFORE INSERT ON tr_t FOR EACH ROW EXECUTE FUNCTION tr_upper()
INSERT INTO tr_t(id, name, n) VALUES (1, 'alpha', 5), (2, 'beta', 10)
SELECT id, name, n, touched FROM tr_t ORDER BY id
CREATE TRIGGER t_skip BEFORE INSERT ON tr_t FOR EACH ROW EXECUTE FUNCTION tr_skip()
INSERT INTO tr_t(id, name, n) VALUES (3, 'gamma', -1), (4, 'delta', 7)
SELECT id, name, n FROM tr_t ORDER BY id
CREATE TRIGGER t_audit AFTER INSERT OR UPDATE OR DELETE ON tr_t FOR EACH ROW EXECUTE FUNCTION tr_audit()
INSERT INTO tr_t(id, name, n) VALUES (5, 'eps', 1)
SELECT op, tname, lvl, whn, old_id, new_id, info FROM tr_log ORDER BY seq
CREATE TRIGGER t_stamp BEFORE UPDATE ON tr_t FOR EACH ROW EXECUTE FUNCTION tr_stamp()
UPDATE tr_t SET n = n + 1 WHERE id <= 2
SELECT id, name, n, touched FROM tr_t ORDER BY id
UPDATE tr_t SET name = 'zeta' WHERE id = 5
SELECT id, name, touched FROM tr_t WHERE id = 5
SELECT op, old_id, new_id, info FROM tr_log WHERE op = 'UPDATE' ORDER BY seq
CREATE TRIGGER t_guard BEFORE DELETE ON tr_t FOR EACH ROW EXECUTE FUNCTION tr_guard()
UPDATE tr_t SET n = 500 WHERE id = 4
DELETE FROM tr_t WHERE id = 4
DELETE FROM tr_t WHERE id = 1
SELECT id FROM tr_t ORDER BY id
SELECT op, old_id FROM tr_log WHERE op = 'DELETE' ORDER BY seq
CREATE TRIGGER t_stmt AFTER DELETE ON tr_t FOR EACH STATEMENT EXECUTE FUNCTION tr_stmt()
DELETE FROM tr_t WHERE id = 999
SELECT op, lvl, whn FROM tr_log WHERE lvl = 'STATEMENT' ORDER BY seq
CREATE TRIGGER t_upper BEFORE INSERT ON tr_t FOR EACH ROW EXECUTE FUNCTION tr_upper()
CREATE OR REPLACE TRIGGER t_upper BEFORE INSERT ON tr_t FOR EACH ROW EXECUTE FUNCTION tr_upper()
CREATE TRIGGER t_bad BEFORE INSERT ON tr_t FOR EACH ROW EXECUTE FUNCTION tr_notrig()
CREATE TRIGGER t_missing BEFORE INSERT ON tr_t FOR EACH ROW EXECUTE FUNCTION no_such_fn()
CREATE TRIGGER t_notab BEFORE INSERT ON no_such_table FOR EACH ROW EXECUTE FUNCTION tr_upper()
DROP TRIGGER t_upper ON tr_t
DROP TRIGGER t_upper ON tr_t
DROP TRIGGER IF EXISTS t_upper ON tr_t
INSERT INTO tr_t(id, name, n) VALUES (6, 'lower', 1)
SELECT id, name FROM tr_t WHERE id = 6
CREATE TRIGGER t_when BEFORE UPDATE ON tr_t FOR EACH ROW WHEN (NEW.n > 50) EXECUTE FUNCTION tr_upper()
UPDATE tr_t SET n = 60 WHERE id = 6
UPDATE tr_t SET n = 2, name = 'small' WHERE id = 2
SELECT id, name, n FROM tr_t ORDER BY id
UPDATE tr_t SET n = n WHERE id = 2 RETURNING id, name, touched
SELECT tgname FROM pg_trigger WHERE NOT tgisinternal AND tgrelid = 'tr_t'::regclass ORDER BY tgname
DROP FUNCTION tr_upper()
