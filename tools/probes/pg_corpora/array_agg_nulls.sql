SELECT array_agg(v) FROM aa
SELECT array_agg(v) FROM aa WHERE false
SELECT array_agg(v) FROM aa WHERE id=2
SELECT array_agg(s) FROM aa
SELECT string_agg(s, ',') FROM aa
SELECT string_agg(s, ',') FROM aa WHERE false
SELECT string_agg(s, ',') FROM aa WHERE id=2
SELECT json_agg(v) FROM aa
SELECT json_agg(v) FROM aa WHERE false
SELECT jsonb_agg(v) FROM aa WHERE id=2
SELECT b.id, array_agg(a.v) FROM ab b LEFT JOIN aa a ON a.id=b.id AND a.id<3 GROUP BY b.id ORDER BY b.id
SELECT array_agg(v ORDER BY id DESC) FROM aa
SELECT array_agg(DISTINCT v) FROM aa
SELECT array_agg(v) FILTER (WHERE id>99) FROM aa
SELECT array_agg(v) FILTER (WHERE id<3) FROM aa
