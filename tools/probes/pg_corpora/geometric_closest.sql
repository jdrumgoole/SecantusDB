# reference-version: 15
# The closest-point operators ## (six pairs) and <-> between a line or box and a segment, from geo_ops.c (RP-015).
SELECT point '(0,0)' ## lseg '[(1,1),(2,2)]', point '(3,0)' ## lseg '[(0,0),(2,0)]', point '(1,5)' ## lseg '[(0,0),(2,0)]'
SELECT point '(0,0)' ## box '(1,1),(2,2)', point '(1.5,5)' ## box '(1,1),(2,2)', point '(1.5,1.5)' ## box '(1,1),(2,2)'
SELECT point '(0,0)' ## line '{1,-1,1}', point '(2,3)' ## line '{0,1,-1}'
SELECT lseg '[(0,0),(1,1)]' ## box '(2,2),(3,3)', lseg '[(0,5),(5,5)]' ## box '(1,1),(2,2)'
SELECT lseg '[(0,0),(1,1)]' ## lseg '[(3,0),(3,5)]', lseg '[(0,0),(2,0)]' ## lseg '[(1,1),(1,5)]'
SELECT line '{1,-1,0}' ## lseg '[(2,0),(3,0)]', lseg '[(2,0),(3,0)]' ## line '{1,-1,0}'
SELECT line '{1,-1,0}' <-> lseg '[(2,0),(3,0)]', lseg '[(2,0),(3,0)]' <-> line '{1,-1,0}', box '(0,0),(1,1)' <-> lseg '[(3,3),(4,4)]', lseg '[(3,3),(4,4)]' <-> box '(0,0),(1,1)'
SELECT line '{1,-1,0}' <-> box '(2,0),(3,1)', box '(2,0),(3,1)' <-> line '{1,-1,0}', line '{1,-1,0}' <-> line '{1,-1,5}', line '{1,-1,0}' <-> line '{1,1,0}'
SELECT point '(3,1)' ## lseg '[(0,0),(4,4)]', point '(-1,-3)' ## lseg '[(0,0),(4,4)]', point '(1,2)' ## lseg '[(0,4),(4,0)]'
SELECT point '(5,1)' ## box '(1,1),(4,3)', point '(0,0)' ## box '(1,1),(4,3)', point '(2,1)' ## box '(1,1),(4,3)'
SELECT lseg '[(0,0),(5,5)]' ## box '(1,3),(2,4)', lseg '[(0,0),(1,0)]' ## box '(2,2),(3,3)', lseg '[(-5,2),(5,2)]' ## box '(-1,-1),(1,1)'
SELECT lseg '[(0,0),(4,4)]' ## lseg '[(0,4),(4,0)]', lseg '[(0,0),(1,0)]' ## lseg '[(2,1),(5,1)]', lseg '[(0,0),(1,2)]' ## lseg '[(3,0),(4,1)]'
SELECT line '{1,1,-4}' ## lseg '[(0,0),(1,0)]', line '{0,1,-2}' ## lseg '[(0,0),(4,4)]', point '(1,1)' ## line '{1,1,0}'
SELECT line '{1,1,-4}' <-> lseg '[(0,0),(1,0)]', box '(1,1),(2,2)' <-> lseg '[(0,0),(5,0)]', lseg '[(0,0),(5,5)]' <-> box '(1,3),(2,4)'
SELECT pg_typeof(point '(0,0)' ## box '(1,1),(2,2)'), pg_typeof(box '(1,1),(2,2)' <-> lseg '[(0,0),(5,0)]')
