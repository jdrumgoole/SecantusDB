DROP EVENT TRIGGER IF EXISTS ev_start
DROP EVENT TRIGGER IF EXISTS ev_end
DROP EVENT TRIGGER IF EXISTS ev_drop
DROP EVENT TRIGGER IF EXISTS evp_d
DROP EVENT TRIGGER IF EXISTS evp_e
DROP TABLE IF EXISTS ev_a, ev_b, ev_log, evp_log, evp_t, rc_t, rc_s CASCADE
DROP VIEW IF EXISTS evp_v
DROP SEQUENCE IF EXISTS evp_s
DROP TYPE IF EXISTS evp_ty
DROP FUNCTION IF EXISTS ev_f(), ev_g(), evp_g(), evp_c(), evp_bad(), evp_f(int, text), df_a(), df_b(int)
