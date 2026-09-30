CREATE TYPE ea_m AS ENUM ('a', 'b')
ALTER TYPE ea_m ADD VALUE 'b'
ALTER TYPE ea_m ADD VALUE IF NOT EXISTS 'b'
ALTER TYPE ea_m ADD VALUE 'z' BEFORE 'nope'
ALTER TYPE ea_m ADD VALUE 'c' AFTER 'a'
ALTER TYPE ea_m ADD VALUE 'first' BEFORE 'a'
ALTER TYPE ea_m ADD VALUE 'last'
CREATE TABLE ea_t (x ea_m, xs ea_m[])
INSERT INTO ea_t VALUES ('c', '{c,b}'), ('b', '{a}')
ALTER TYPE ea_m RENAME VALUE 'c' TO 'cc'
SELECT x::text, xs::text FROM ea_t ORDER BY x
SELECT x FROM ea_t WHERE x > 'a' ORDER BY x
ALTER TYPE ea_m RENAME VALUE 'nope' TO 'q'
ALTER TYPE ea_m RENAME VALUE 'a' TO 'b'
SELECT enum_range(NULL::ea_m), enum_first(NULL::ea_m), enum_last(NULL::ea_m), pg_typeof(enum_range(NULL::ea_m))
SELECT enum_range('a'::ea_m, 'b'::ea_m), enum_range(NULL, 'cc'::ea_m)
SELECT enumlabel FROM pg_enum e JOIN pg_type t ON t.oid = e.enumtypid WHERE t.typname = 'ea_m' ORDER BY enumsortorder
ALTER TYPE nosuch_enum ADD VALUE 'x'
