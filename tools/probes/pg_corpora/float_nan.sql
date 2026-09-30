# NaN is greater than every number, Infinity included, and equal to itself
# (float8, float4 and numeric) -- in filters, sorts and aggregates.
SELECT id FROM fn WHERE f > 1e308 ORDER BY id
SELECT id FROM fn WHERE f > 'Infinity' ORDER BY id
SELECT id FROM fn WHERE f >= 1 ORDER BY id
SELECT id FROM fn WHERE f < 'NaN' ORDER BY id
SELECT id FROM fn WHERE f <= 'NaN' ORDER BY id
SELECT id FROM fn WHERE f = 'NaN' ORDER BY id
SELECT id FROM fn WHERE f <> 'NaN' ORDER BY id
SELECT id FROM fn WHERE f > 'NaN' ORDER BY id
SELECT id FROM fn WHERE f >= 'NaN' ORDER BY id
SELECT id FROM fn WHERE f BETWEEN 0 AND 'NaN' ORDER BY id
SELECT id FROM fn WHERE r > 1 ORDER BY id
SELECT id FROM fn WHERE n > 1 ORDER BY id
SELECT id FROM fn WHERE n = 'NaN' ORDER BY id
SELECT id, f FROM fn ORDER BY f, id
SELECT id, f FROM fn ORDER BY f DESC, id
SELECT id FROM fn ORDER BY n, id
SELECT max(f), min(f), max(n), min(n) FROM fn
SELECT count(DISTINCT f) FROM fn
SELECT 'NaN'::float8 > 'Infinity'::float8, 'NaN'::float8 = 'NaN'::float8, 'NaN'::numeric > 1e100
SELECT greatest(1, 'NaN'::float8), least(1, 'NaN'::float8)
