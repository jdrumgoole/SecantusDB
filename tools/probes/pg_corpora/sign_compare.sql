# reference-version: 15
# The comparison functions whose magnitude is not promised answer only its
# sign (Joe, 2026-10-16); the corpus compares signs. bttextcmp / bpcharcmp /
# jsonb_cmp by the database collation's order, tsvector_cmp / tsquery_cmp by
# their storage layouts (silly_cmp_tsvector, CompareTSQ).
SELECT sign(bttextcmp('b', 'a')), sign(bttextcmp('a', 'b')), bttextcmp('a', 'a'), bpcharcmp('a ', 'a'), sign(bpcharcmp('ab', 'a'))
SELECT sign(tsvector_cmp('a b', 'a')), sign(tsvector_cmp('a:1', 'a')), sign(tsvector_cmp('a:1A', 'a:1B')), sign(tsvector_cmp('a:2', 'a:1')), sign(tsvector_cmp('ab', 'ba')), tsvector_cmp('b a', 'a b')
SELECT sign(tsvector_cmp('a:1,2', 'a:3')), sign(tsvector_cmp('xyz', 'x y z')), sign(tsvector_cmp('a:16383', 'b:1')), sign(tsvector_cmp('é', 'e'))
SELECT sign(tsquery_cmp('a & b', 'b & a')), sign(tsquery_cmp('a | b', 'a & b')), sign(tsquery_cmp('!a', 'a')), sign(tsquery_cmp('a <-> b', 'a <2> b')), tsquery_cmp('a:*', 'a:A')
SELECT sign(tsquery_cmp('a & b & c', '(a | b) & c')), sign(tsquery_cmp('ab', 'abc')), sign(tsquery_cmp('d & e', 'e & d')), sign(tsquery_cmp('zzz | y', 'x'))
