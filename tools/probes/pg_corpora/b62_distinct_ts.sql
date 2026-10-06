# DISTINCT over timestamp / timestamptz output columns, streamed in bounded
# memory (batch 62): a timestamp's identity is its millisecond date AND its
# hidden sub-millisecond companion, sorted right after it. Outside a block
# with no WHERE (the streamed path), inside one through DECLARE CURSOR.
SELECT DISTINCT ts FROM b62_ts ORDER BY ts
SELECT DISTINCT ts FROM b62_ts ORDER BY ts DESC
SELECT DISTINCT ts FROM b62_ts ORDER BY ts DESC NULLS LAST
SELECT DISTINCT tz FROM b62_ts ORDER BY tz
SELECT DISTINCT tz, k FROM b62_ts ORDER BY k, tz DESC
SELECT DISTINCT k, ts FROM b62_ts ORDER BY k, ts LIMIT 9
SELECT DISTINCT ts, tz FROM b62_ts ORDER BY tz, ts OFFSET 3 LIMIT 10
SELECT count(*) FROM (SELECT DISTINCT ts FROM b62_ts) s
SELECT count(*) FROM (SELECT DISTINCT ts, tz FROM b62_ts) s
SELECT id, ts FROM b62_ts ORDER BY ts DESC, id LIMIT 12
SELECT id, tz FROM b62_ts ORDER BY tz, id LIMIT 12
BEGIN
INSERT INTO b62_ts VALUES (2000, '2024-01-01 00:00:00.000003', '2024-01-01 00:00:00.000001+00', 9)
DECLARE c1 CURSOR FOR SELECT DISTINCT ts FROM b62_ts ORDER BY ts
FETCH 6 FROM c1
FETCH 6 FROM c1
CLOSE c1
DECLARE c2 CURSOR FOR SELECT DISTINCT tz FROM b62_ts ORDER BY tz DESC
FETCH ALL FROM c2
ROLLBACK
# The same orders through the materialised path (a WHERE, read whole).
SELECT id, ts FROM b62_ts WHERE id > 0 ORDER BY ts DESC, id LIMIT 12
SELECT id, tz FROM b62_ts WHERE id > 0 ORDER BY tz, id LIMIT 12
SELECT DISTINCT ts FROM b62_ts WHERE id > 0 ORDER BY ts DESC LIMIT 8
SELECT DISTINCT ON (ts) ts, id FROM b62_ts ORDER BY ts, id DESC LIMIT 8
SELECT DISTINCT ON (tz) tz, id FROM b62_ts ORDER BY tz DESC, id
# Window peers and ranks over sub-millisecond timestamps (the same comparison).
SELECT id, rank() OVER (ORDER BY ts) FROM b62_ts WHERE id % 50 = 1 ORDER BY id
SELECT id, count(*) OVER (ORDER BY tz DESC) FROM b62_ts WHERE id % 40 = 3 ORDER BY id
SELECT ts, count(*) FROM b62_ts GROUP BY ts ORDER BY ts DESC LIMIT 5
SELECT min(ts), max(ts), min(tz), max(tz) FROM b62_ts
SELECT min(ts), max(ts) FROM b62_ts WHERE id > 0
SELECT k, min(ts), max(tz) FROM b62_ts GROUP BY k ORDER BY k
SELECT count(DISTINCT ts), count(DISTINCT tz) FROM b62_ts
SELECT k, count(DISTINCT ts) FROM b62_ts GROUP BY k ORDER BY k
SELECT DISTINCT ON (ts) ts, id FROM b62_ts WHERE id > 0 ORDER BY ts DESC, id LIMIT 6
SELECT ts, count(*) FROM b62_ts WHERE id > 10 GROUP BY ROLLUP (ts) ORDER BY ts LIMIT 4
