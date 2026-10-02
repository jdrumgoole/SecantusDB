# reference-version: 15
# money: cash_in / cash_out, casts, arithmetic, aggregates, cash_words.
SELECT '12.34'::money, '$1,234.5'::money, pg_typeof('1'::money)
SELECT '12.345'::money, '12.355'::money, '-12.345'::money, '(5)'::money, '$-1,0.1'::money, ' 3 '::money
SELECT 'abc'::money
SELECT '1e3'::money
SELECT 1.5::float8::money
SELECT '92233720368547758.07'::money
SELECT '92233720368547758.08'::money
SELECT '-92233720368547758.08'::money
SELECT 12.345::numeric::money, (-0.005)::numeric::money, 7::money, 7::int8::money
SELECT '1.23'::money::text, '1.23'::money::numeric, '5'::money - '7'
SELECT '1'::money + '1', '5'::money - '0.5', pg_typeof('1'::money + '1')
SELECT '10'::money / 3, '10'::money / 3.0, '10'::money * 1.5, 2 * '3'::money, '10'::money / '4'::money, '-10'::money / 3, '1.01'::money / 2
SELECT pg_typeof('10'::money / '4'::money), pg_typeof(2 * '3'::money)
SELECT '1'::money / 0
SELECT '1'::money < '2'::money, '1'::money = '1.00'::money
SELECT id, m FROM b38_m ORDER BY id
SELECT max(m), min(m), sum(m) FROM b38_m
SELECT avg(m) FROM b38_m
SELECT id FROM b38_m WHERE m > '0' ORDER BY id
SELECT m::text FROM b38_m ORDER BY id
SELECT cash_words('1.01'::money), cash_words('12.34'::money), cash_words('1000'::money), cash_words('0.01'::money)
SELECT cash_words('-112.10'::money), cash_words('1234567.89'::money), cash_words('92233720368547758.07'::money), cash_words('20.20'::money), cash_words('110.11'::money)
SELECT %s::money ||| ['4.25']
DROP TABLE b38_m
