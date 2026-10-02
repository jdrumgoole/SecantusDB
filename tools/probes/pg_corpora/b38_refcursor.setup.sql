DROP FUNCTION IF EXISTS b38_get()
DROP FUNCTION IF EXISTS b38_out(int4)
DROP FUNCTION IF EXISTS b38_named(refcursor)
DROP FUNCTION IF EXISTS b38_bound()
DROP TABLE IF EXISTS b38_rc
CREATE TABLE b38_rc (id int, v text)
INSERT INTO b38_rc VALUES (1, 'a'), (2, 'b'), (3, 'c')
CREATE FUNCTION b38_get() RETURNS refcursor AS 'declare r refcursor; begin open r for select id from b38_rc order by id; return r; end;' LANGUAGE plpgsql
CREATE FUNCTION b38_out(p_cur OUT refcursor, p_lim int4) AS $$ BEGIN OPEN p_cur FOR SELECT v FROM b38_rc ORDER BY id LIMIT p_lim; END; $$ LANGUAGE plpgsql
CREATE FUNCTION b38_named(c refcursor) RETURNS refcursor AS $$ BEGIN OPEN c FOR EXECUTE 'select v from b38_rc where id > ' || 1 || ' order by id'; RETURN c; END; $$ LANGUAGE plpgsql
CREATE FUNCTION b38_bound() RETURNS refcursor AS $$ DECLARE c CURSOR FOR SELECT id * 10 FROM b38_rc ORDER BY id; BEGIN OPEN c; RETURN c; END; $$ LANGUAGE plpgsql
