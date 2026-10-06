# reference-version: 15
# avg and float sum with no GROUP BY, read a chunk at a time (batch 61): the
# float total is carried from chunk to chunk, so the answer is the one-pass
# one to the last bit; run also with SECANTUS_PG_GROUP_MEMORY_BYTES=1000.
SELECT sum(d), avg(d), count(d) FROM b61_f
SELECT sum(r), avg(r) FROM b61_f WHERE id < 1500
SELECT avg(r) FROM b61_f
SELECT avg(i), avg(b), avg(n), avg(s) FROM b61_f
SELECT avg(i), sum(d), max(d), min(r) FROM b61_f WHERE pad LIKE 'x%'
SELECT sum(d), avg(r), avg(n) FROM b61_f0
SELECT avg(d) FROM b61_f WHERE id > 3990
SELECT sum(d) * 2, avg(n) + 1 FROM b61_f
SELECT avg(i), sum(d) FROM b61_f HAVING count(*) > 10
SELECT sum(r) FROM b61_f WHERE s < 100
