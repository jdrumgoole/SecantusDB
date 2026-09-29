# --- a set-returning function as the FROM item
SELECT * FROM unnest(ARRAY[1,2,3])
SELECT * FROM unnest(ARRAY['a','b']) AS t(s)
SELECT x FROM unnest(ARRAY[3,1,2]) x
SELECT array_agg(x) FROM unnest(ARRAY[3,1,2]) x
SELECT count(*) FROM unnest(ARRAY[1,2,3])
SELECT x * 2 FROM unnest(ARRAY[1,2]) x
SELECT * FROM unnest(ARRAY[]::int[])
SELECT * FROM unnest(NULL::int[])
SELECT * FROM unnest(ARRAY[[1,2],[3,4]])
# --- ordering, filtering and limiting over one
SELECT x FROM unnest(ARRAY[3,1,2]) x ORDER BY x
SELECT x FROM unnest(ARRAY[3,1,2]) x WHERE x > 1 ORDER BY x
SELECT x FROM unnest(ARRAY[3,1,2]) x ORDER BY x LIMIT 2
# --- generate_series still works beside it
SELECT * FROM generate_series(1,3)
SELECT g FROM generate_series(1,3) g WHERE g > 1
# --- other set-returning functions in FROM
SELECT * FROM regexp_split_to_table('a,b,c', ',')
SELECT * FROM generate_subscripts(ARRAY[5,6,7], 1)
# --- in the SELECT list (the harder half)
SELECT unnest(ARRAY[1,2])
SELECT unnest(ia) FROM sr1 WHERE id=1
SELECT generate_subscripts(ARRAY[5,6,7], 1)
