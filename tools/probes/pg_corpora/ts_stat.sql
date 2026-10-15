# reference-version: 15
# ts_stat(query [, weights]): ndoc / nentry per lexeme, the weights mask, NULL rows, the one-tsvector-column check, in FROM, ROWS FROM, LATERAL and a select list (RP-015).
select * from ts_stat('select v from tst_t') order by 1
select * from ts_stat('select v from tst_t', 'ab') order by 1
select * from ts_stat('select v from tst_t', 'd') order by 1
select * from ts_stat('select v from tst_t', 'x') order by 1
select * from ts_stat('select v from tst_t', 'AbCd') order by 1
select * from ts_stat('select t from tst_t')
select * from ts_stat('select v, v from tst_t')
select * from ts_stat('select v from tst_t where false')
select word, nentry from ts_stat('select to_tsvector(''english'', ''The cats sat on the cats mat'')') order by 2 desc, 1
select (ts_stat('select v from tst_t')).word order by 1
select * from ts_stat(null)
select count(*) from ts_stat('select v from tst_t', null)
select pg_typeof(ndoc)::text, pg_typeof(word)::text from ts_stat('select v from tst_t') limit 1
select * from rows from (ts_stat('select v from tst_t')) order by 1
select * from rows from (ts_parse('default', 'a b'))
select * from (select 1) s, lateral ts_stat('select v from tst_t') order by 2
