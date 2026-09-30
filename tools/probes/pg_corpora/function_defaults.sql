# A parameter's DEFAULT fills a call that leaves it out; it is printed as
# ruleutils prints it, and a later parameter without one is refused.
SELECT fdx(1), fdx(1, 'y'), fdx(1, 'y', 7), fdx(1, 'y', 7, 2.5)
SELECT fdp(), fdp(3)
SELECT fdx()
SELECT pg_get_function_arguments(p.oid), pg_get_function_identity_arguments(p.oid), pg_get_function_result(p.oid) FROM pg_proc p WHERE proname = 'fdx'
CREATE FUNCTION fdx_bad(a int DEFAULT 1, b int) RETURNS int LANGUAGE sql AS 'select a'
DROP FUNCTION fdx(int, text, int, numeric)
DROP FUNCTION fdp(int)
