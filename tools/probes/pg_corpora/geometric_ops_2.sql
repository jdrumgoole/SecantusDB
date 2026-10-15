# reference-version: 15
# Geometric operators that were refused: point <>, box # / <^ / >^, the circle position tests, # and ?# over lines and segments (RP-015).
SELECT box '(0,0),(2,2)' # box '(1,1),(3,3)', box '(0,0),(1,1)' # box '(2,2),(3,3)'
SELECT box '(0,0),(1,1)' <^ box '(0,2),(1,3)', box '(0,2),(1,3)' >^ box '(0,0),(1,1)', box '(0,0),(1,1)' <^ box '(0,0),(1,3)'
SELECT circle '<(0,0),1>' &< circle '<(3,0),1>', circle '<(0,0),1>' &> circle '<(3,0),1>', circle '<(0,0),1>' &<| circle '<(0,3),1>', circle '<(0,3),1>' |&> circle '<(0,0),1>'
SELECT line '{1,-1,0}' # line '{1,1,0}', line '{1,-1,0}' # line '{1,-1,5}', line '{1,-1,0}' ?# line '{1,1,0}', line '{1,-1,0}' ?# line '{1,-1,5}'
SELECT lseg '[(0,0),(2,2)]' ?# lseg '[(0,2),(2,0)]', lseg '[(0,0),(1,1)]' ?# lseg '[(5,5),(6,7)]'
SELECT point '(1,1)' <> point '(1,2)', point '(1,1)' <> point '(1,1)'
SELECT pg_typeof(box '(0,0),(2,2)' # box '(1,1),(3,3)'), pg_typeof(line '{1,-1,0}' # line '{1,1,0}')
