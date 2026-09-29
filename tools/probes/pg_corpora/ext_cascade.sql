DROP EXTENSION hstore
SELECT isempty('[1,2)')
SELECT isempty('[1,2)'::int4range)
SELECT lower_inc(NULL)
DROP EXTENSION hstore CASCADE
SELECT * FROM ec_t
SELECT h FROM ec_t
SELECT column_name FROM information_schema.columns WHERE table_name = 'ec_u' ORDER BY 1
SELECT count(*) FROM pg_extension WHERE extname = 'hstore'
CREATE EXTENSION hstore
