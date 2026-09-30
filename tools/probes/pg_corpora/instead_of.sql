INSERT INTO io_v VALUES (3, 'C')
CREATE TRIGGER io_trg INSTEAD OF INSERT OR UPDATE OR DELETE ON io_v FOR EACH ROW EXECUTE FUNCTION io_f()
INSERT INTO io_v VALUES (3, 'C'), (4, 'D')
INSERT INTO io_v (uname, id) VALUES ('E', 5)
SELECT * FROM io_t ORDER BY id
UPDATE io_v SET uname = 'ZZ' WHERE id = 2
UPDATE io_v SET uname = uname || '!' WHERE id > 3
SELECT * FROM io_t ORDER BY id
DELETE FROM io_v WHERE id = 1
DELETE FROM io_v WHERE id = 42
SELECT * FROM io_t ORDER BY id
SELECT msg FROM io_log ORDER BY msg
CREATE TRIGGER io_bad INSTEAD OF INSERT ON io_t FOR EACH ROW EXECUTE FUNCTION io_f()
CREATE TRIGGER io_bad2 BEFORE INSERT ON io_v FOR EACH ROW EXECUTE FUNCTION io_f()
CREATE TRIGGER io_bad3 INSTEAD OF INSERT ON io_v FOR EACH STATEMENT EXECUTE FUNCTION io_f()
CREATE TRIGGER io_bad4 INSTEAD OF UPDATE OF uname ON io_v FOR EACH ROW EXECUTE FUNCTION io_f()
SELECT tgname FROM pg_trigger WHERE tgrelid = 'io_v'::regclass
DROP TRIGGER io_trg ON io_v
INSERT INTO io_v VALUES (8, 'H')
SELECT relname, relkind, relnatts FROM pg_class WHERE relname IN ('io_v', 'io_t') ORDER BY 1
SELECT 'io_v'::regclass::text
SELECT c.relname FROM pg_class c WHERE c.oid = 'io_v'::regclass
