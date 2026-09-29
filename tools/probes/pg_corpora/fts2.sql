SELECT id FROM ts2 WHERE tv @@ to_tsquery('english', 'cat') ORDER BY id
SELECT id FROM ts2 WHERE tv @@ to_tsquery('english', 'cat & !dog') ORDER BY id
SELECT id FROM ts2 WHERE tv @@ to_tsquery('english', 'dog <-> chase') ORDER BY id
SELECT id FROM ts2 WHERE tv @@ to_tsquery('english', 'chase <-> dog') ORDER BY id
SELECT id FROM ts2 WHERE tv @@ 'cat' ORDER BY id
SELECT id FROM ts2 WHERE body @@ 'cats' ORDER BY id
SELECT id FROM ts2 WHERE body @@ to_tsquery('mice | mat') ORDER BY id
SELECT id, tv @@ to_tsquery('cat') FROM ts2 ORDER BY id
SELECT id, length(tv) FROM ts2 ORDER BY id
SELECT id, tv FROM ts2 ORDER BY id
SELECT to_tsvector('english', 'cat sat mat') @@ to_tsquery('english', 'cat <2> mat')
SELECT to_tsvector('english', 'cat sat mat') @@ to_tsquery('english', 'cat <-> !sat')
SELECT to_tsvector('english', 'cat bat mat') @@ to_tsquery('english', 'cat <-> !sat')
SELECT to_tsvector('english', 'cat sat mat') @@ to_tsquery('english', '(cat | dog) <-> sat')
SELECT to_tsvector('english', 'cat sat mat') @@ to_tsquery('english', 'cat <-> (sat <-> mat)')
SELECT to_tsvector('english', 'cat sat mat') @@ to_tsquery('english', 'ca:*')
SELECT to_tsvector('english', 'cat sat mat') @@ to_tsquery('english', 'ca:* <-> sa:*')
SELECT strip(to_tsvector('english', 'cat sat mat')) @@ to_tsquery('english', 'cat <-> sat')
SELECT strip(to_tsvector('english', 'cat sat mat')) @@ to_tsquery('english', 'sat <-> cat')
SELECT setweight(to_tsvector('english', 'cat sat'), 'A') @@ to_tsquery('english', 'cat:A')
SELECT setweight(to_tsvector('english', 'cat sat'), 'A') @@ to_tsquery('english', 'cat:B')
SELECT to_tsvector('english', 'cat sat') @@ to_tsquery('english', 'cat:D')
SELECT setweight(to_tsvector('english', 'cat sat mat'), 'B', ARRAY['cat', 'mat'])
SELECT setweight('a:1,2 b:3'::tsvector, 'c') || setweight('c:1'::tsvector, 'a')
SELECT ts_delete(to_tsvector('english', 'cat sat mat'), 'sat')
SELECT ts_delete(to_tsvector('english', 'cat sat mat'), ARRAY['sat', 'cat'])
SELECT ts_filter('a:1A b:2B c:3'::tsvector, '{a,b}')
SELECT 'a:1 a:1 b:5,3,3'::tsvector
SELECT $$'\\x' 'it''s' 'a b'$$::tsvector
SELECT 'fat:*AB & !rat:C'::tsquery
SELECT 'a <3> b'::tsquery
SELECT 'a & (b | c) & d'::tsquery
SELECT 'a | b & c'::tsquery
SELECT '(a | b) <-> c'::tsquery
SELECT 'a <-> (b <-> c)'::tsquery
SELECT '!!a'::tsquery
SELECT '!(a & b)'::tsquery
SELECT 'a & '::tsquery
SELECT '& a'::tsquery
SELECT '(a'::tsquery
SELECT 'a <x> b'::tsquery
SELECT ''::tsquery
SELECT ''::tsvector
SELECT to_tsvector('english', 'joe@example.com visited example.com and http://foo.org/x/y')
SELECT to_tsvector('english', 'v1.2.3 is 3.5 times 1e10 or -42')
SELECT to_tsvector('english', 'state-of-the-art multi-level')
SELECT to_tsvector('english', 'Ünïcode naïve résumé')
SELECT to_tsvector('english', '')
SELECT to_tsvector('english', 'the a an')
SELECT to_tsquery('english', 'state-of-the-art')
SELECT to_tsquery('english', '''supernovae stars'' & !crab')
SELECT to_tsquery('english', 'fat:ab & cats:*')
SELECT plainto_tsquery('english', 'State-of-the-art 42 ideas!')
SELECT phraseto_tsquery('english', 'the the cat')
SELECT websearch_to_tsquery('english', 'cat or or dog')
SELECT websearch_to_tsquery('english', 'or cat')
SELECT websearch_to_tsquery('english', '"cat dog" or -"fish bowl"')
SELECT websearch_to_tsquery('english', 'the')
SELECT websearch_to_tsquery('simple', 'a & b | !c')
SELECT to_tsquery('french', 'chat')
SELECT to_tsquery('nosuch', 'chat')
SELECT to_tsvector('pg_catalog.english', 'cats')
SELECT 'english'::regconfig
SELECT get_current_ts_config()
SELECT numnode(''::tsquery), querytree('a & !b | c'::tsquery), querytree('!a | b'::tsquery)
SELECT tsquery_phrase('a'::tsquery, 'b'::tsquery, 3)
SELECT 'a'::tsquery <-> 'b'::tsquery
SELECT ts_headline('english', 'The fat cat sat on the mat with another fat cat', to_tsquery('english', 'cat'))
SELECT ts_headline('The quick brown fox', to_tsquery('fox'))
SELECT array_to_tsvector(ARRAY['a', NULL])
SELECT array_to_tsvector(ARRAY['a', ''])
SELECT to_tsvector(NULL), to_tsquery(NULL)
SELECT 'a:0'::tsvector
SELECT 'a:20000'::tsvector
SELECT ts_rank(to_tsvector('english', 'The quick brown fox jumps over the lazy dog'), to_tsquery('english', 'fox'))
SELECT ts_rank(to_tsvector('english', 'The quick brown fox jumps over the lazy dog'), to_tsquery('english', 'fox & dog'))
SELECT ts_rank(to_tsvector('english', 'The quick brown fox jumps over the lazy dog'), to_tsquery('english', 'fox | cat'))
SELECT ts_rank(to_tsvector('english', 'fox fox fox dog'), to_tsquery('english', 'fox'))
SELECT ts_rank(to_tsvector('english', 'fox fox fox dog'), to_tsquery('english', 'fox <-> dog'))
SELECT ts_rank(setweight(to_tsvector('english', 'fox'), 'A') || to_tsvector('english', 'dog'), to_tsquery('english', 'fox & dog'))
SELECT ts_rank('{0.1, 0.2, 0.4, 1.0}', to_tsvector('english', 'fox dog'), to_tsquery('english', 'fox'))
SELECT ts_rank(to_tsvector('english', 'The quick brown fox jumps over the lazy dog'), to_tsquery('english', 'fox & dog'), 1)
SELECT ts_rank(to_tsvector('english', 'The quick brown fox jumps over the lazy dog'), to_tsquery('english', 'fox & dog'), 2)
SELECT ts_rank(to_tsvector('english', 'The quick brown fox jumps over the lazy dog'), to_tsquery('english', 'fox & dog'), 32)
SELECT ts_rank(strip(to_tsvector('english', 'fox dog')), to_tsquery('english', 'fox & dog'))
SELECT ts_rank(to_tsvector('english', 'foxes foxhound dog'), to_tsquery('english', 'fox:*'))
SELECT ts_rank(to_tsvector('english', 'a b c'), to_tsquery('english', 'zzz'))
SELECT id, ts_rank(tv, to_tsquery('english', 'cat')) FROM ts2 ORDER BY id
SELECT ts_rank_cd(to_tsvector('english', 'The quick brown fox jumps over the lazy dog'), to_tsquery('english', 'fox'))
SELECT ts_rank_cd(to_tsvector('english', 'The quick brown fox jumps over the lazy dog'), to_tsquery('english', 'fox & dog'))
SELECT ts_rank_cd(to_tsvector('english', 'fox dog fox cat dog fox'), to_tsquery('english', 'fox & dog'))
SELECT ts_rank_cd(to_tsvector('english', 'fox dog fox cat dog fox'), to_tsquery('english', 'fox <-> dog'))
SELECT ts_rank_cd(to_tsvector('english', 'fox dog fox cat dog fox'), to_tsquery('english', 'fox | cat'))
SELECT ts_rank_cd(to_tsvector('english', 'fox dog fox cat dog fox'), to_tsquery('english', 'fox & dog'), 4)
SELECT ts_rank_cd(to_tsvector('english', 'fox dog fox cat dog fox'), to_tsquery('english', 'fox & dog'), 1)
SELECT ts_rank_cd(to_tsvector('english', 'fox dog fox cat dog fox'), to_tsquery('english', 'fox & !cat'))
SELECT ts_rank_cd('{0.1, 0.2, 0.4, 1.0}', setweight(to_tsvector('english', 'fox'), 'A') || to_tsvector('english', 'x y dog'), to_tsquery('english', 'fox & dog'))
SELECT ts_rank_cd(strip(to_tsvector('english', 'fox dog')), to_tsquery('english', 'fox & dog'))
