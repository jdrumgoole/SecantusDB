SELECT corr(y, x) FROM st22
SELECT covar_pop(y, x), covar_samp(y, x) FROM st22
SELECT regr_slope(y, x), regr_intercept(y, x), regr_r2(y, x) FROM st22
SELECT regr_count(y, x), regr_avgx(y, x), regr_avgy(y, x) FROM st22
SELECT regr_sxx(y, x), regr_syy(y, x), regr_sxy(y, x) FROM st22
SELECT g, corr(y, x) FROM st22 GROUP BY g ORDER BY g
SELECT g, regr_count(y, x) FROM st22 GROUP BY g ORDER BY g
SELECT corr(y, x) FROM st22 WHERE id = 1
SELECT corr(y, x) FROM st22 WHERE false
SELECT regr_count(y, x) FROM st22 WHERE false
SELECT covar_samp(y, x) FROM st22 WHERE id = 1
SELECT corr(y, x) FILTER (WHERE id < 4) FROM st22
SELECT regr_count(y, x) FILTER (WHERE id < 3) FROM st22
SELECT corr(y, x) FROM st22 WHERE id = 5
SELECT regr_slope(y, x) FROM st22 HAVING regr_count(y, x) > 2
SELECT round(corr(y, x)::numeric, 6) FROM st22
SELECT corr(id, id) FROM st22
