# reference-version: 15
# macaddr / macaddr8 & | ~ (and by name), and the path count comparisons (RP-015).
SELECT path '((0,0),(1,1))' < path '((0,0),(1,1),(2,2))', path '((0,0),(1,1))' = path '((0,0),(2,2))', path '((0,0),(1,1))' >= path '((0,0),(2,2))'
SELECT '08:00:2b:01:02:03'::macaddr & 'ff:ff:ff:00:00:00'::macaddr
SELECT '08:00:2b:01:02:03'::macaddr | '00:00:00:ff:ff:ff'::macaddr
SELECT ~ '08:00:2b:01:02:03'::macaddr
SELECT '08:00:2b:01:02:03:04:05'::macaddr8 & 'ff:ff:ff:00:00:00:00:00'::macaddr8, ~ '08:00:2b:01:02:03:04:05'::macaddr8, '08:00:2b:01:02:03:04:05'::macaddr8 | '00:00:00:00:00:00:00:ff'::macaddr8
SELECT pg_typeof(~ '08:00:2b:01:02:03'::macaddr), macaddr_and('08:00:2b:01:02:03', 'ff:ff:ff:00:00:00'), macaddr_not('08:00:2b:01:02:03')
