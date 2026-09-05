# LATERAL over a set-returning function. Every line here is `0A000 unsupported
# LATERAL source` today; see tasks/backlog.md for the full diagnosis of what
# the planner needs (the executor is already able to run these).
SELECT t.id, x FROM sc21 t, LATERAL unnest(t.arr) AS x ORDER BY t.id, x
SELECT t.id, x FROM sc21 t CROSS JOIN LATERAL unnest(t.arr) AS x ORDER BY t.id, x
SELECT t.id, x FROM sc21 t LEFT JOIN LATERAL unnest(t.arr) AS x ON true ORDER BY t.id, x
SELECT t.id, g FROM sc21 t, LATERAL generate_series(1, t.id) AS g ORDER BY t.id, g
SELECT t.id, e FROM sc21 t, LATERAL jsonb_array_elements(t.j->'a'->'b') AS e ORDER BY t.id
SELECT t.id, y.v FROM sc21 t, LATERAL (SELECT t.id * 2 AS v) y ORDER BY t.id
SELECT t.id, p FROM sc21 t, LATERAL regexp_split_to_table(t.s, ',') AS p ORDER BY t.id, p
SELECT count(*) FROM sc21 t, LATERAL unnest(t.arr) AS x
SELECT t.id FROM sc21 t, LATERAL unnest(t.arr) AS x WHERE x > 1 ORDER BY t.id
