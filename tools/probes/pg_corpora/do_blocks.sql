DO $$ BEGIN PERFORM 1/0; EXCEPTION WHEN division_by_zero THEN RAISE NOTICE 'caught'; END $$
DO $$ DECLARE i int; BEGIN FOR i IN 1..3 LOOP INSERT INTO do_t VALUES (i, 'v' || i); END LOOP; END $$
SELECT id, v FROM do_t ORDER BY id
DO $$ DECLARE c int; BEGIN SELECT count(*) INTO c FROM do_t; IF c <> 3 THEN RAISE EXCEPTION 'count %', c; END IF; END $$
DO $$ DECLARE r record; s text := ''; BEGIN FOR r IN SELECT v FROM do_t ORDER BY id LOOP s := s || r.v; END LOOP; INSERT INTO do_t VALUES (10, s); END $$
SELECT v FROM do_t WHERE id = 10
DO $$ DECLARE n int := 0; BEGIN WHILE n < 5 LOOP n := n + 1; END LOOP; UPDATE do_t SET v = n::text WHERE id = 1; END $$
SELECT v FROM do_t WHERE id = 1
DO $$ BEGIN RAISE EXCEPTION 'boom %', 42 USING ERRCODE = '22012'; END $$
DO $$ DECLARE x int; BEGIN x := 'abc'; END $$
DO $$ BEGIN INSERT INTO do_t VALUES (1, 'dup'); EXCEPTION WHEN unique_violation THEN UPDATE do_t SET v = 'handled' WHERE id = 1; END $$
SELECT v FROM do_t WHERE id = 1
DO $$ DECLARE a int[] := ARRAY[1,2,3]; t int := 0; x int; BEGIN FOREACH x IN ARRAY a LOOP t := t + x; END LOOP; UPDATE do_t SET v = t::text WHERE id = 2; END $$
SELECT v FROM do_t WHERE id = 2
DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM do_t WHERE id = 99) THEN INSERT INTO do_t VALUES (99, 'new'); END IF; END $$
SELECT v FROM do_t WHERE id = 99
