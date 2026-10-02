# reference-version: 15
# Batch 45: ruleutils prints cross-type date/time comparisons, [NOT]
# MATERIALIZED CTEs and RANGE offsets over number / date-time orderings
# instead of falling back; RANGE offsets are checked against the ordering.
SELECT pg_get_viewdef('b45v1')
SELECT pg_get_viewdef('b45v2')
SELECT pg_get_viewdef('b45v3')
SELECT pg_get_viewdef('b45v4')
SELECT pg_get_viewdef('b45v5')
SELECT pg_get_viewdef('b45v6')
SELECT pg_get_viewdef('b45v8')
SELECT pg_get_viewdef('b45v1', true)
SELECT pg_get_viewdef('b45v4', true)
SELECT pg_get_viewdef('b45v9')
SELECT pg_get_viewdef('b45v10')
SELECT pg_get_viewdef('b45v11')
SELECT pg_get_viewdef('b45v12')
SELECT sum(a) OVER (ORDER BY d RANGE BETWEEN 1 PRECEDING AND CURRENT ROW) FROM b45r
SELECT sum(a) OVER (ORDER BY t RANGE BETWEEN 1 PRECEDING AND CURRENT ROW) FROM b45r
SELECT sum(a) OVER (ORDER BY a RANGE BETWEEN 'x' PRECEDING AND CURRENT ROW) FROM b45r
SELECT sum(a) OVER (ORDER BY a, n RANGE BETWEEN 1 PRECEDING AND CURRENT ROW) FROM b45r
SELECT sum(a) OVER w FROM b45r WINDOW w AS (ORDER BY t RANGE BETWEEN 1 PRECEDING AND CURRENT ROW)
SELECT sum(a) OVER (ORDER BY ts RANGE BETWEEN '1 hour' PRECEDING AND CURRENT ROW) FROM b45r
SELECT sum(a) OVER (ORDER BY ts RANGE BETWEEN interval '1 hour' PRECEDING AND '2 hours' FOLLOWING) FROM b45r
SELECT sum(a) OVER (ORDER BY n RANGE BETWEEN 1 PRECEDING AND 1.5 FOLLOWING) FROM b45r
SELECT sum(a) OVER (ORDER BY a ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM b45r
SELECT sum(a) OVER (RANGE BETWEEN 1 PRECEDING AND CURRENT ROW) FROM b45r
