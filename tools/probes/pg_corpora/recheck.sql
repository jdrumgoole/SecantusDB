SET timezone = 'Europe/Berlin'
SELECT ('2030-01-01 12:00:00+02'::timestamptz::timestamp)::text
SET timezone = 'UTC'
SELECT 1::no_such_type
SELECT count(*) FROM generate_series(1,5) i WHERE i > 2
SELECT upper(s) FROM rc_t ORDER BY id
SELECT regexp_replace(s, '[0-9]', 'X') AS s FROM rc_t ORDER BY id
SELECT id FROM rc_t WHERE b ORDER BY id
SELECT id FROM rc_t WHERE NOT b ORDER BY id
SELECT '(1,2),(3,4)'::box = '(3,4),(1,2)'::box
SELECT '(0,0),(1,1)'::box = '(0,0),(2,0.5)'::box
SELECT (array[1,2])[1], ('{1,2}'::int[])[2], (array[1,2,3])[2:3], (array[1,2])[5]
SELECT (ts + interval '1 day')::text FROM rc_t WHERE id = 1
SELECT pg_sleep(0) IS NULL
SELECT coalesce(pg_sleep(0)::text, 'x')
SELECT 'ok' FROM pg_sleep(0)
DEALLOCATE nosuch
DROP TABLE IF EXISTS nosuch_rc
SELECT '(1,2),(3,4)'::box = '(1,2),(3,4)'::box
SELECT '(1,2),(3,4)'::box < '(1,2),(3,4)'::box
SELECT '(1,2),(3,4)'::box <= '(1,2),(3,4)'::box
SELECT '(1,2),(3,4)'::box > '(1,2),(3,4)'::box
SELECT '(1,2),(3,4)'::box >= '(1,2),(3,4)'::box
SELECT '(1,2),(3,4)'::box ~= '(1,2),(3,4)'::box
SELECT '(1,2),(3,4)'::box && '(1,2),(3,4)'::box
SELECT '(1,2),(3,4)'::box @> '(1,2),(3,4)'::box
SELECT '(1,2),(3,4)'::box <@ '(1,2),(3,4)'::box
SELECT '(1,2),(3,4)'::box << '(1,2),(3,4)'::box
SELECT '(1,2),(3,4)'::box >> '(1,2),(3,4)'::box
SELECT '(1,2),(3,4)'::box &< '(1,2),(3,4)'::box
SELECT '(1,2),(3,4)'::box &> '(1,2),(3,4)'::box
SELECT '(1,2),(3,4)'::box <<| '(1,2),(3,4)'::box
SELECT '(1,2),(3,4)'::box |>> '(1,2),(3,4)'::box
SELECT '(1,2),(3,4)'::box &<| '(1,2),(3,4)'::box
SELECT '(1,2),(3,4)'::box |&> '(1,2),(3,4)'::box
SELECT '(1,2),(3,4)'::box = '(0,0),(1,1)'::box
SELECT '(1,2),(3,4)'::box < '(0,0),(1,1)'::box
SELECT '(1,2),(3,4)'::box <= '(0,0),(1,1)'::box
SELECT '(1,2),(3,4)'::box > '(0,0),(1,1)'::box
SELECT '(1,2),(3,4)'::box >= '(0,0),(1,1)'::box
SELECT '(1,2),(3,4)'::box ~= '(0,0),(1,1)'::box
SELECT '(1,2),(3,4)'::box && '(0,0),(1,1)'::box
SELECT '(1,2),(3,4)'::box @> '(0,0),(1,1)'::box
SELECT '(1,2),(3,4)'::box <@ '(0,0),(1,1)'::box
SELECT '(1,2),(3,4)'::box << '(0,0),(1,1)'::box
SELECT '(1,2),(3,4)'::box >> '(0,0),(1,1)'::box
SELECT '(1,2),(3,4)'::box &< '(0,0),(1,1)'::box
SELECT '(1,2),(3,4)'::box &> '(0,0),(1,1)'::box
SELECT '(1,2),(3,4)'::box <<| '(0,0),(1,1)'::box
SELECT '(1,2),(3,4)'::box |>> '(0,0),(1,1)'::box
SELECT '(1,2),(3,4)'::box &<| '(0,0),(1,1)'::box
SELECT '(1,2),(3,4)'::box |&> '(0,0),(1,1)'::box
SELECT '(1,2),(3,4)'::box = '(0,0),(2,0.5)'::box
SELECT '(1,2),(3,4)'::box < '(0,0),(2,0.5)'::box
SELECT '(1,2),(3,4)'::box <= '(0,0),(2,0.5)'::box
SELECT '(1,2),(3,4)'::box > '(0,0),(2,0.5)'::box
SELECT '(1,2),(3,4)'::box >= '(0,0),(2,0.5)'::box
SELECT '(1,2),(3,4)'::box ~= '(0,0),(2,0.5)'::box
SELECT '(1,2),(3,4)'::box <> '(1,2),(3,4)'::box
