CREATE TRIGGER t1_args BEFORE INSERT ON tg_a FOR EACH ROW EXECUTE FUNCTION tg_args('x', 'y')
CREATE TRIGGER t2_noargs BEFORE INSERT ON tg_a FOR EACH ROW EXECUTE PROCEDURE tg_args()
INSERT INTO tg_a(id, v, amt) VALUES (1, 'one', 3)
SELECT msg FROM tg_log ORDER BY seq
DROP TRIGGER t1_args ON tg_a
DROP TRIGGER t2_noargs ON tg_a
CREATE TRIGGER t_fail BEFORE INSERT OR UPDATE ON tg_a FOR EACH ROW EXECUTE FUNCTION tg_fail()
INSERT INTO tg_a(id, v, amt) VALUES (2, 'ok', 1), (3, 'bad', 1)
SELECT id FROM tg_a ORDER BY id
UPDATE tg_a SET v = 'bad' WHERE id = 1
SELECT id, v FROM tg_a ORDER BY id
CREATE TRIGGER t_ts BEFORE INSERT ON tg_a FOR EACH ROW EXECUTE FUNCTION tg_ts()
INSERT INTO tg_a(id, v, amt) VALUES (4, 'four', 4.25), (5, 'five', 7)
SELECT id, amt, at, flag FROM tg_a WHERE id >= 4 ORDER BY id
INSERT INTO tg_a(id, v, amt) VALUES (6, 'six', 1) RETURNING id, amt, at, flag
CREATE TRIGGER t_cnt_b BEFORE INSERT ON tg_a FOR EACH STATEMENT EXECUTE FUNCTION tg_cnt()
CREATE TRIGGER t_cnt_a AFTER INSERT ON tg_a FOR EACH STATEMENT EXECUTE FUNCTION tg_cnt()
DELETE FROM tg_log
INSERT INTO tg_a(id, v, amt) VALUES (7, 'seven', 1), (8, 'eight', 1)
SELECT msg FROM tg_log ORDER BY seq
CREATE TRIGGER t_upd_of AFTER UPDATE OF amt ON tg_a FOR EACH ROW EXECUTE FUNCTION tg_args('amt')
DELETE FROM tg_log
UPDATE tg_a SET v = 'x' WHERE id = 7
SELECT msg FROM tg_log ORDER BY seq
UPDATE tg_a SET amt = 9 WHERE id = 7
SELECT msg FROM tg_log ORDER BY seq
INSERT INTO tg_b VALUES (1, 7, 'n1'), (2, 7, 'n2'), (3, 8, 'n3')
CREATE TRIGGER t_casc BEFORE DELETE ON tg_a FOR EACH ROW EXECUTE FUNCTION tg_cascade()
DELETE FROM tg_a WHERE id = 7
SELECT id, a_id FROM tg_b ORDER BY id
CREATE TRIGGER t_trunc AFTER TRUNCATE ON tg_b FOR EACH STATEMENT EXECUTE FUNCTION tg_cnt()
DELETE FROM tg_log
TRUNCATE tg_b
SELECT msg FROM tg_log ORDER BY seq
CREATE TRIGGER t_row_trunc AFTER TRUNCATE ON tg_b FOR EACH ROW EXECUTE FUNCTION tg_cnt()
CREATE TRIGGER t_stmt_when AFTER INSERT ON tg_b FOR EACH STATEMENT WHEN (NEW.id > 1) EXECUTE FUNCTION tg_cnt()
BEGIN
INSERT INTO tg_a(id, v, amt) VALUES (20, 'tx', 1)
ROLLBACK
SELECT count(*) FROM tg_a WHERE id = 20
SELECT count(*) FROM tg_log WHERE msg LIKE '%count=%'
DROP TABLE tg_b
SELECT count(*) FROM pg_trigger WHERE tgname = 't_trunc'
SELECT tgname, tgtype, tgnargs, tgenabled FROM pg_trigger WHERE tgname IN ('t_casc', 't_cnt_a', 't_cnt_b', 't_fail', 't_ts', 't_upd_of', 't_trunc') ORDER BY tgname
