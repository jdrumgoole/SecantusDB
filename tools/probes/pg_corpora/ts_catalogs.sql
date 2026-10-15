# reference-version: 15
# The text-search catalogs, regconfig / regdictionary by name and by oid, ts_lexize, and default_text_search_config (RP-015).
SELECT oid, cfgname, cfgnamespace, cfgowner, cfgparser FROM pg_ts_config ORDER BY oid
SELECT oid, dictname, dictnamespace, dicttemplate, dictinitoption FROM pg_ts_dict ORDER BY oid
SELECT prsname, prsstart, prstoken::text, prsend, prsheadline, prslextype FROM pg_ts_parser
SELECT tmplname, tmplinit, tmpllexize FROM pg_ts_template ORDER BY oid
SELECT count(*), count(DISTINCT mapcfg), count(DISTINCT mapdict) FROM pg_ts_config_map
SELECT c.cfgname, m.maptokentype, d.dictname FROM pg_ts_config_map m JOIN pg_ts_config c ON c.oid = m.mapcfg JOIN pg_ts_dict d ON d.oid = m.mapdict WHERE c.cfgname IN ('english', 'russian') ORDER BY 1, 2
SELECT cfgname FROM pg_ts_config WHERE oid = 'german'::regconfig
SELECT 3748::regconfig, 3748::regconfig::text, 'english'::regconfig::oid, 'pg_catalog.german'::regconfig::text, '"simple"'::regconfig::oid, 99999::regconfig
SELECT 3765::regdictionary, 'simple'::regdictionary::oid, 'english_stem'::regdictionary::oid
SELECT pg_typeof('english'::regconfig), pg_typeof('simple'::regdictionary), pg_typeof(get_current_ts_config())
SELECT to_tsvector(3748::regconfig, 'The cats'), to_tsvector('english'::regconfig, 'The cats ran'), to_tsquery(13636::regconfig, 'cats & ran')
SELECT 'nope'::regconfig
SELECT 'nope'::regdictionary
SELECT to_tsvector('nope', 'a b')
SELECT ts_lexize('english_stem', 'cats'), ts_lexize('english_stem', 'the'), ts_lexize('simple', 'The'), ts_lexize(13637::regdictionary, 'running')
SELECT ts_lexize('german_stem', 'Häuser'), ts_lexize('russian_stem', 'книги'), ts_lexize('french_stem', 'le'), ts_lexize('swedish_stem', 'och')
SELECT ts_lexize('nope', 'x')
SELECT ts_lexize('simple', NULL), pg_typeof(ts_lexize('simple', 'a'))
SHOW default_text_search_config
SELECT get_current_ts_config(), get_current_ts_config()::oid
SET default_text_search_config = 'German'
SHOW default_text_search_config
SELECT get_current_ts_config(), to_tsvector('Häuser und Bäume'), plainto_tsquery('Häuser')
SET default_text_search_config = 'nope'
SET default_text_search_config = '"simple"'
SHOW default_text_search_config
SELECT to_tsvector('The cats'), get_current_ts_config()::oid
SET default_text_search_config = 'pg_catalog.english'
SELECT to_tsvector('The cats')
