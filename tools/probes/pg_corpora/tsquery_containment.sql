# reference-version: 15
# tsquery @> / <@ (tsq_mcontains / tsq_mcontained), by operator and by name (RP-015).
SELECT 'a & b'::tsquery @> 'a'::tsquery, 'a'::tsquery <@ 'a & b'::tsquery, 'a'::tsquery @> 'a & b'::tsquery
SELECT 'a | b'::tsquery @> 'b & a'::tsquery, 'a & b & c'::tsquery @> 'c | d'::tsquery, '!a'::tsquery @> 'a'::tsquery
SELECT tsq_mcontains('a & b', 'a'), tsq_mcontained('a', 'a & b')
SELECT tsq_mcontains('a & b', 'a')
SELECT tsq_mcontains('a & b'::tsquery, 'a'::tsquery)
SELECT 'a & b' @> 'a'::tsquery
SELECT 'a'::tsquery <@ 'a & b'
