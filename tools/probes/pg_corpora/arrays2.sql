# --- the dimension family, over an empty / NULL / multidimensional array
SELECT array_length(ia,1), array_ndims(ia), array_dims(ia), cardinality(ia) FROM a12 WHERE id=2
SELECT array_length(ta,1), array_ndims(ta), array_dims(ta), cardinality(ta) FROM a12 WHERE id=3
SELECT array_upper(m,1), array_upper(m,2), array_lower(m,1), array_lower(m,3) FROM a12 WHERE id=1
SELECT array_length(ia,0), array_length(ia,2), array_length(ia,NULL) FROM a12 WHERE id=1
# --- the NULL rules, which differ per function
SELECT array_cat(NULL::int[], ARRAY[3]), array_cat(ARRAY[1], NULL::int[]), array_cat(NULL::int[], NULL::int[])
SELECT array_append(NULL::int[], 2), array_append(ARRAY[1], NULL::int), array_prepend(NULL::int, ARRAY[1])
SELECT array_remove(NULL::int[], 1), array_replace(NULL::int[], 1, 2)
SELECT array_position(ia, NULL), array_positions(ia, NULL) FROM a12 WHERE id=3
SELECT array_remove(ia, NULL), array_replace(ia, NULL, 9) FROM a12 WHERE id=3
# --- searching
SELECT array_position(ARRAY[1,2,3,2], 2), array_position(ARRAY[1,2,3,2], 2, 3), array_position(ARRAY[1,2,3,2], 2, 9)
SELECT array_position(ARRAY[1,2], 9), array_positions(ARRAY[1,2], 9)
# --- text conversion corners
SELECT array_to_string(ia, '-'), array_to_string(ia, '-', 'NIL') FROM a12 WHERE id=3
SELECT array_to_string(m, ':'), array_to_string(ta, ',') FROM a12 WHERE id=1
SELECT string_to_array('a,b', NULL), string_to_array('a,b', ''), string_to_array('', 'x')
SELECT string_to_array('aXXbXXc', 'XX'), string_to_array('a,b,c', ',', 'b')
# --- containment, which does NOT match NULL to NULL
SELECT ia @> ARRAY[NULL]::int[], ia <@ ia, ia && ARRAY[NULL]::int[] FROM a12 WHERE id=3
SELECT ARRAY[1,2] @> ARRAY[]::int[], ARRAY[]::int[] @> ARRAY[]::int[], ARRAY[1] @> NULL::int[]
SELECT m @> ARRAY[3], m && ARRAY[9,4], ARRAY[1] <@ m FROM a12 WHERE id=1
# --- subscripting, including through a column-valued index
SELECT ia[n], ia[n:3], ia[1:n] FROM a12 WHERE id=1
SELECT m[1], m[1][2], m[2:2], m[1:2][2] FROM a12 WHERE id=1
SELECT ia[1], ia[1:2] FROM a12 WHERE id=2
SELECT (ARRAY[1,2,3])[0], (ARRAY[1,2,3])[0:1], (ARRAY[1,2,3])[3:1]
SELECT ta[1] || ta[2], length(ta[1]) FROM a12 WHERE id=1
# --- assignment into an array
UPDATE a12 SET ia[2] = 99 WHERE id=1 RETURNING ia
UPDATE a12 SET ia[6] = 6 WHERE id=1 RETURNING ia
UPDATE a12 SET ia[1] = 7, ia[2] = 8 WHERE id=1 RETURNING ia
UPDATE a12 SET ia[2:3] = ARRAY[4,5] WHERE id=1 RETURNING ia
UPDATE a12 SET ia[1] = 1 WHERE id=2 RETURNING ia
UPDATE a12 SET m[1][2] = 42 WHERE id=1 RETURNING m
UPDATE a12 SET ia[n] = 0 WHERE id=1 RETURNING ia
SELECT ia, m FROM a12 WHERE id=1
SELECT ia FROM a12 WHERE id=2
