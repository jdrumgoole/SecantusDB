# reference-version: 15
# A STABLE / IMMUTABLE function runs read-only (batch 63): any statement
# but a plain SELECT is 0A000 "... is not allowed in a non-volatile
# function" -- at a SQL function's startup (nothing runs), per statement in
# PL/pgSQL. A VOLATILE function it calls may still write.
SELECT b63_s_ins()
SELECT b63_i_upd()
SELECT b63_s_cte()
SELECT b63_s_lock()
SELECT b63_s_show()
SELECT b63_s_read()
SELECT b63_v_ins()
SELECT b63_p_ins()
SELECT b63_p_del()
SELECT b63_p_ddl()
SELECT b63_p_trunc()
SELECT b63_p_set()
SELECT b63_p_exec()
SELECT b63_p_forupd()
SELECT b63_p_forshare()
SELECT b63_p_notify()
SELECT b63_p_read()
SELECT b63_p_calls_v()
SELECT b63_v_calls_s()
SELECT x FROM b63_nv ORDER BY x
SELECT b63_s_read() FROM b63_nv ORDER BY x
BEGIN
SELECT b63_p_ins()
ROLLBACK
SELECT x FROM b63_nv ORDER BY x
