# --- interval arithmetic and normalisation
SELECT iv, -iv, iv * 2, iv / 2 FROM dt9
SELECT iv + iv, iv - iv FROM dt9
SELECT INTERVAL '1 mon' + INTERVAL '30 days', INTERVAL '1 day' - INTERVAL '25 hours'
SELECT INTERVAL '1.5 months', INTERVAL '1.5 days', INTERVAL '-1.5 hours'
SELECT INTERVAL '1 year 13 months', INTERVAL '25:00:00', INTERVAL '90 minutes'
SELECT justify_interval(INTERVAL '1 mon 35 days 30 hours')
SELECT extract(epoch FROM INTERVAL '1 day 1 hour'), extract(day FROM INTERVAL '40 days')
SELECT extract(month FROM INTERVAL '14 months'), extract(year FROM INTERVAL '14 months')
SELECT INTERVAL '1 day' = INTERVAL '24 hours', INTERVAL '1 mon' = INTERVAL '30 days'
SELECT INTERVAL '1 day' < INTERVAL '25 hours'
SELECT date_trunc('hour', INTERVAL '1 day 3:45:00')
# --- date/time arithmetic
SELECT d + 1, d - 1, d + INTERVAL '1 day', d - d FROM dt9
SELECT ts + INTERVAL '1 mon', ts - INTERVAL '1 mon' FROM dt9
SELECT ts - ts, ts::date, ts::time FROM dt9
SELECT tm + INTERVAL '1 hour', tm - INTERVAL '14 hours' FROM dt9
SELECT age(DATE '2021-03-01', DATE '2020-02-29')
SELECT age(TIMESTAMP '2020-03-31', TIMESTAMP '2020-01-31')
SELECT DATE '2020-01-31' + INTERVAL '1 month', DATE '2020-03-31' + INTERVAL '-1 month'
SELECT TIMESTAMP '2020-02-29 12:00' + INTERVAL '1 year'
SELECT to_char(INTERVAL '1 day 2 hours', 'HH24:MI:SS'), to_char(DATE '2020-01-05', 'Day')
SELECT to_char(TIMESTAMP '2020-01-05 13:45', 'YYYY-MM-DD HH12:MI AM')
SELECT to_timestamp(0), to_timestamp(1600000000)
SELECT to_date('2020-02-29', 'YYYY-MM-DD'), to_date('29/02/2020','DD/MM/YYYY')
SELECT date_part('epoch', TIMESTAMP '1970-01-01')
SELECT TIMESTAMP 'epoch', DATE 'epoch'
SELECT TIMESTAMP '2020-01-01' - TIMESTAMP '2019-01-01'
SELECT (DATE '2020-03-01' - DATE '2020-01-01') * INTERVAL '1 day'
SELECT make_timestamp(2020,2,29,13,45,56.789)
SELECT extract(dow FROM DATE '2020-02-29'), extract(week FROM DATE '2020-12-31')
SELECT DATE '2020-02-29' + 366
SELECT tm, tm::interval FROM dt9
SELECT LOCALTIME IS NOT NULL, CURRENT_DATE IS NOT NULL
SELECT INTERVAL '2 days' > INTERVAL '47 hours'
