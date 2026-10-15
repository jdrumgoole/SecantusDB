# reference-version: 15
# RP-015's smaller items: segment and box distances at the float range, "char" ordering by byte, unlogged tables in pg_class and pg_relation_is_publishable.
SELECT pg_relation_is_publishable('smu'::regclass), pg_relation_is_publishable('sm2'::regclass)
SELECT point '(1e300,1e300)' <-> point '(-1e300,-1e300)', point '(3e-310,0)' <-> point '(0,4e-310)', lseg '[(0,0),(1e300,1e300)]' <-> point '(0,1e300)'
SELECT circle '<(1e300,1e300),1>' <-> point '(-1e300,-1e300)', box '(1e300,1e300),(0,0)' <-> point '(-1e300,-1e300)'
SELECT c from (values ('a'::"char"), (E'\\351'::"char"), ('z'::"char"), (E'\\201'::"char")) v(c) order by c
SELECT c from sm2 order by c desc
SELECT c from (values ('b'::"char"), (''::"char"), ('A'::"char")) v(c) order by c
SELECT relname, relpersistence from pg_class where relname in ('smu', 'sm2') order by 1
