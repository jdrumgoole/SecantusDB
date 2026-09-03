SELECT ia[1], ia[2:3], ia[0], ia[9] FROM a11 WHERE id=1
SELECT m[1][2], m[2][1], m[1:2][1:1] FROM a11 WHERE id=1
SELECT array_dims(m), array_ndims(m), array_length(m,1), array_length(m,2) FROM a11 WHERE id=1
SELECT cardinality(m), array_upper(m,2), array_lower(m,2) FROM a11 WHERE id=1
SELECT ia || ia, ta || 'c', 0 || ia FROM a11 WHERE id=1
SELECT array_cat(m, ARRAY[[5,6]]) FROM a11 WHERE id=1
SELECT unnest(ia) FROM a11 WHERE id=1
SELECT unnest(m) FROM a11 WHERE id=1
SELECT array_agg(x) FROM unnest(ARRAY[3,1,2]) x
SELECT array_to_string(m, ',') FROM a11 WHERE id=1
SELECT ia @> ARRAY[1], ia <@ ARRAY[1,2,3,4], ia && ARRAY[3,9] FROM a11 WHERE id=1
SELECT array_position(ia, 2), array_positions(ia, 2) FROM a11 WHERE id=1
SELECT array_remove(ia, 2), array_replace(ia, 2, 9) FROM a11 WHERE id=1
SELECT array_fill(0, ARRAY[2,2])
SELECT array_fill(7, ARRAY[2], ARRAY[3])
SELECT ARRAY(SELECT n FROM (VALUES(1),(2)) t(n))
SELECT ARRAY[1,2] = ARRAY[1,2], ARRAY[1,2] < ARRAY[1,3], ARRAY[1] < ARRAY[1,2]
SELECT ARRAY[NULL,1]::int[], array_length(ARRAY[NULL]::int[], 1)
SELECT ia IS NULL, ta IS NULL FROM a11 WHERE id=2
SELECT array_length(ia,1) FROM a11 WHERE id=2
SELECT string_to_array('a,b,,c', ','), string_to_array(NULL, ',')
SELECT '{1,2,3}'::int[], '{{1,2},{3,4}}'::int[][]
SELECT '{a,b}'::text[] @> '{a}'::text[]
SELECT ia[2:] , ia[:2] FROM a11 WHERE id=1
UPDATE a11 SET ia[2] = 99 WHERE id = 1 RETURNING ia
UPDATE a11 SET ia[5] = 5 WHERE id = 1 RETURNING ia
SELECT ia FROM a11 WHERE id=1
SELECT generate_subscripts(ARRAY[5,6,7], 1)
SELECT * FROM unnest(ARRAY[1,2], ARRAY['a','b']) AS t(n, s)
