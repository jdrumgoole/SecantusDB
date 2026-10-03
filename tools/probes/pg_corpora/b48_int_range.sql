# reference-version: 15
# Batch 48: a bigint too large for an int4 / int2 target is 22003 with
# PostgreSQL's own message (it was 22P02 naming the value) -- the error a
# prepared UPDATE / INSERT binding an int8 to an int4 column gets.
SELECT 1099511627776::int8::int4
SELECT 1099511627776::int8::int2
SELECT (-1099511627776)::int8::int4
SELECT 2147483647::int8::int4
