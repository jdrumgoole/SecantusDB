# What PostgreSQL 15 has and 14 did not (this server reports 15.0), and the
# SQL/JSON syntax 15 does NOT have, answered as 15 answers it.
# reference-version: 15
# --- regexp_count / regexp_instr / regexp_substr / regexp_like
SELECT regexp_count('abcabc','b'), regexp_count('abcabc','b',3), regexp_count('ABab','a',1,'i'), regexp_count('aaa','aa'), regexp_count('abc','')
SELECT regexp_instr('abcabc','c'), regexp_instr('abcabc','c',1,2), regexp_instr('abcabc','c',1,3), regexp_instr('abcabc','b(c)',1,1,1), regexp_instr('abcabc','b(c)',1,1,0,'',1), regexp_instr('abcabc','b(c)',1,2,1,'i',1)
SELECT regexp_substr('abcabc','b.'), regexp_substr('abcabc','b.',3), regexp_substr('abcabc','b(.)',1,2,'',1), regexp_substr('abc','z') IS NULL, regexp_substr('ABC','b',1,1,'i')
SELECT regexp_count('héllo', 'l', 3), regexp_instr('héllo', 'l')
SELECT regexp_count('abc','b',0)
SELECT regexp_count('abc','b',1,'g')
SELECT regexp_count('abc','b',10)
SELECT regexp_instr('abc','b',1,0)
SELECT regexp_instr('abc','b',1,1,2)
SELECT regexp_instr('abc','b',1,1,0,'',-1)
SELECT regexp_instr('abc','(b)',1,1,0,'',5)
SELECT regexp_like('abc','B'), regexp_like('abc','B','i'), regexp_like(NULL,'a') IS NULL
SELECT regexp_like('abc','b','g')
SELECT pg_typeof(regexp_count('a','a')), pg_typeof(regexp_instr('a','a')), pg_typeof(regexp_substr('a','a'))
SELECT regexp_count('a.c','.'), regexp_substr('abc', '(a)(b)?', 1, 1, '', 2)
# --- UNIQUE NULLS NOT DISTINCT
INSERT INTO unn VALUES (NULL, 1, NULL)
INSERT INTO unn VALUES (NULL, 2, NULL)
INSERT INTO unn VALUES (1, 1, NULL)
INSERT INTO unn VALUES (2, NULL, NULL)
INSERT INTO unn VALUES (3, NULL, NULL)
SELECT count(*) FROM unn
INSERT INTO unx VALUES (NULL), (NULL)
CREATE UNIQUE INDEX unx_a ON unx (a) NULLS NOT DISTINCT
DELETE FROM unx
CREATE UNIQUE INDEX unx_a ON unx (a) NULLS NOT DISTINCT
INSERT INTO unx VALUES (NULL)
INSERT INTO unx VALUES (NULL)
SELECT conname, pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid = 'unn'::regclass ORDER BY 1
SELECT indexdef FROM pg_indexes WHERE indexname = 'unx_a'
INSERT INTO und VALUES (NULL), (NULL)
CREATE UNIQUE INDEX uio_i ON uio (a) INCLUDE (b) NULLS NOT DISTINCT WHERE a > 0
SELECT indexdef FROM pg_indexes WHERE indexname = 'uio_i'
# --- SQL/JSON syntax from 16 and 17
SELECT JSON_ARRAY(1,2)
SELECT JSON_OBJECT('a': 1)
SELECT JSON_OBJECT()
SELECT json_serialize('1')
SELECT JSON_OBJECTAGG('a': x) FROM (VALUES (1)) v(x)
SELECT JSON_QUERY('{}', '$')
SELECT JSON_EXISTS('{}', '$')
SELECT JSON_VALUE('{}', '$')
SELECT * FROM JSON_TABLE('[]', '$' COLUMNS (a int))
SELECT '1' IS JSON OBJECT
SELECT json_scalar(1)
SELECT JSON_OBJECT('a' VALUE 1)
SELECT merge_action()
# --- range_agg / range_intersect_agg over multiranges (15)
SELECT range_agg(m)::text FROM (VALUES (int4multirange(int4range(1,2))), (int4multirange(int4range(4,6)))) v(m)
SELECT range_intersect_agg(m)::text FROM (VALUES (int4multirange(int4range(1,5))), (int4multirange(int4range(3,9)))) v(m)
# --- moved from the PostgreSQL 14 corpora (14 lacks these)
SELECT regexp_split_to_array('a1b2c', '\d'), regexp_count('aaa', 'a'), regexp_instr('abc', 'c'), regexp_substr('abc123', '\d+')
SELECT regexp_count('a1b2c3', '[0-9]'), regexp_instr('a1b2', '[0-9]'), regexp_like('abc','b')
SELECT regexp_substr('a1b22c', '[0-9]+'), regexp_substr('a1b22c', '[0-9]+', 1, 2)
