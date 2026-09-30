# reference-version: 15
# pg_trgm: trigram similarity, word similarity, and their operators.
select show_trgm('cat')
select show_trgm('Hello, World! a1b2')
select show_trgm('')
select show_trgm('a')
select show_trgm('  x  y  ')
select similarity('word', 'two words')
select similarity('cat', 'cats'), similarity('', ''), similarity('abc', '')
select 'cat' % 'cats', 'cat' % 'dog'
select 'cat' <-> 'cats'
select word_similarity('word', 'two words'), strict_word_similarity('word', 'two words')
select word_similarity('cat', 'the category'), strict_word_similarity('cat', 'the category')
select word_similarity('abc', 'xabcx abc'), strict_word_similarity('abc', 'xabcx abc')
select word_similarity('Hello', 'hello world'), word_similarity('', 'x'), word_similarity('x', '')
select 'word' <% 'two words', 'two words' %> 'word'
select 'word' <<% 'two words', 'word' <<-> 'two words', 'word' <<<-> 'two words'
select show_trgm('ünïcödé')
select similarity('ünïcödé', 'unicode')
select similarity('Café', 'cafe')
SELECT id FROM tg WHERE w % 'cat' ORDER BY id
SELECT id, w <-> 'cat' AS d FROM tg WHERE w IS NOT NULL ORDER BY d, id
SELECT id, round(similarity(w, 'catalog')::numeric, 4) FROM tg ORDER BY id
SELECT id FROM tg WHERE 'cat' <% w ORDER BY id
SELECT show_limit()
SET pg_trgm.similarity_threshold = 0.6
SELECT id FROM tg WHERE w % 'cat' ORDER BY id
SELECT current_setting('pg_trgm.similarity_threshold')
SELECT set_limit(0.2)
SELECT show_limit()
SELECT id FROM tg WHERE w % 'cat' ORDER BY id
SELECT set_limit(2)
SELECT similarity(NULL, 'x'), word_similarity('x', NULL)
CREATE INDEX tg_g ON tg USING gin (w gin_trgm_ops)
CREATE INDEX tg_s ON tg USING gist (w gist_trgm_ops)
CREATE INDEX tg_b ON tg (w gin_trgm_ops)
SELECT indexdef FROM pg_indexes WHERE tablename = 'tg' ORDER BY 1
SELECT id FROM tg WHERE w % 'cats' ORDER BY id
