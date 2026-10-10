# reference-version: 15
# A relation in `public` is found by its bare name only while `public` is on the search_path; a view created over one keeps reading it.
SELECT t FROM sppub_t ORDER BY 1
SET search_path TO pg_catalog
SELECT t FROM sppub_t ORDER BY 1
SELECT t FROM public.sppub_t ORDER BY 1
SELECT t FROM sppub_v
SELECT t FROM public.sppub_v ORDER BY 1
SELECT t FROM public.sppub_w ORDER BY 1
SELECT (SELECT count(*) FROM public.sppub_v)
SELECT t, (SELECT count(*) FROM public.sppub_v v WHERE v.t = z.t) FROM public.sppub_t z ORDER BY 1
SELECT count(*) FROM public.sppub_t WHERE t IN (SELECT t FROM sppub_v)
SELECT count(*) FROM public.sppub_t z JOIN sppub_t y ON y.t = z.t
WITH sppub_t AS (SELECT 1 AS t) SELECT t FROM sppub_t
SELECT count(*) FROM pg_class WHERE relname = 'sppub_t'
INSERT INTO sppub_t VALUES (9, 'x')
INSERT INTO public.sppub_t SELECT k + 10, t FROM sppub_t
UPDATE sppub_t SET t = 'q'
DELETE FROM sppub_t
TRUNCATE sppub_t
COPY sppub_t TO STDOUT
ALTER TABLE sppub_t ADD COLUMN z int
CREATE INDEX ON sppub_t (t)
COMMENT ON TABLE sppub_t IS 'x'
DROP TABLE sppub_t
DROP VIEW sppub_v
SELECT count(*) FROM public.sppub_t
SET search_path TO ''
SELECT t FROM sppub_t
SET search_path TO "$user"
SELECT t FROM sppub_t
SET search_path TO pg_catalog, public
SELECT count(*) FROM sppub_t
RESET search_path
SELECT count(*) FROM sppub_t
# Still different here: a name looked up from a string, and DROP ... IF EXISTS.
SET search_path TO pg_catalog
SELECT nextval('sppub_s')
SELECT 'sppub_t'::regclass
RESET search_path
