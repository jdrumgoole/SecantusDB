# reference-version: 15
# aggregates with no GROUP BY, read a chunk at a time (batch 60); run also
# with SECANTUS_PG_GROUP_MEMORY_BYTES=1000 so every chunk is tiny.
SELECT count(*), count(i), count(n), count(t) FROM b60_a
SELECT sum(i), sum(b), sum(n), min(i), max(i) FROM b60_a
SELECT min(n), max(n), min(t), max(t), min(c), max(c) FROM b60_a
SELECT bool_and(f), bool_or(f), bool_and(i > -100) FROM b60_a
SELECT sum(i), count(*) FROM b60_a WHERE t LIKE 'v1%'
SELECT max(length(t)), min(i * 2), count(*) + 1 FROM b60_a
SELECT count(*), sum(i), min(t), max(n), bool_or(f) FROM b60_a0
SELECT sum(i) FROM b60_a HAVING count(*) > 10
SELECT sum(i) FROM b60_a HAVING count(*) > 100000
SELECT avg(i), sum(i::float8), count(DISTINCT i % 10) FROM b60_a
SELECT count(*) FILTER (WHERE i > 0), max(t) FROM b60_a
SELECT sum(b) FROM b60_a WHERE id < 3
SELECT sum(n) * 2, max(i) - min(i) FROM b60_a WHERE id > 2000
