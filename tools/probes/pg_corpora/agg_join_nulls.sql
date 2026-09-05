SELECT j.id, sum(k.v) FROM lj20 j LEFT JOIN lk20 k ON k.jid = j.id GROUP BY j.id ORDER BY j.id
SELECT j.id, sum(k.w) FROM lj20 j LEFT JOIN lk20 k ON k.jid = j.id GROUP BY j.id ORDER BY j.id
SELECT j.id, count(k.v) FROM lj20 j LEFT JOIN lk20 k ON k.jid = j.id GROUP BY j.id ORDER BY j.id
SELECT j.id, count(*) FROM lj20 j LEFT JOIN lk20 k ON k.jid = j.id GROUP BY j.id ORDER BY j.id
SELECT j.id, avg(k.v) FROM lj20 j LEFT JOIN lk20 k ON k.jid = j.id GROUP BY j.id ORDER BY j.id
SELECT j.id, min(k.v), max(k.v) FROM lj20 j LEFT JOIN lk20 k ON k.jid = j.id GROUP BY j.id ORDER BY j.id
SELECT j.id, array_agg(k.v) FROM lj20 j LEFT JOIN lk20 k ON k.jid = j.id GROUP BY j.id ORDER BY j.id
SELECT j.id, string_agg(k.v::text, ',') FROM lj20 j LEFT JOIN lk20 k ON k.jid = j.id GROUP BY j.id ORDER BY j.id
SELECT j.id, coalesce(sum(k.v), -1) FROM lj20 j LEFT JOIN lk20 k ON k.jid = j.id GROUP BY j.id ORDER BY j.id
SELECT j.id FROM lj20 j LEFT JOIN lk20 k ON k.jid = j.id GROUP BY j.id HAVING sum(k.v) IS NULL ORDER BY j.id
SELECT j.g, sum(k.v) FROM lj20 j LEFT JOIN lk20 k ON k.jid = j.id GROUP BY j.g ORDER BY j.g
SELECT sum(k.v) FROM lj20 j LEFT JOIN lk20 k ON k.jid = j.id WHERE j.id = 3
