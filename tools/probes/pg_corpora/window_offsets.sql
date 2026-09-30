# RANGE frames measured in the ordering column's own units: an interval over
# a date or time, a fractional number over a number.
SELECT d, sum(i) OVER (ORDER BY d RANGE BETWEEN '1 day' PRECEDING AND CURRENT ROW) FROM wo ORDER BY d, i
SELECT t, sum(i) OVER (ORDER BY t RANGE BETWEEN '12 hours' PRECEDING AND '1 day' FOLLOWING) FROM wo ORDER BY t, i
SELECT i, sum(i) OVER (ORDER BY z DESC RANGE BETWEEN '1 month' PRECEDING AND CURRENT ROW) FROM wo ORDER BY z DESC, i
SELECT d, sum(i) OVER (ORDER BY d DESC RANGE BETWEEN CURRENT ROW AND '3 days' FOLLOWING) FROM wo ORDER BY d DESC, i
SELECT d, count(*) OVER (ORDER BY d RANGE BETWEEN '1 day' PRECEDING AND '1 day' PRECEDING) FROM wo ORDER BY d, i
SELECT f, sum(i) OVER (ORDER BY f RANGE BETWEEN 0.5 PRECEDING AND CURRENT ROW) FROM wo ORDER BY f, i
SELECT f, sum(i) OVER (ORDER BY f RANGE BETWEEN 0.75::float8 PRECEDING AND 0.5::float8 FOLLOWING) FROM wo ORDER BY f, i
SELECT n, sum(i) OVER (ORDER BY n DESC RANGE BETWEEN 0.5 PRECEDING AND 1.25 FOLLOWING) FROM wo ORDER BY n DESC, i
# ROWS and GROUPS take a bigint: a fraction rounds, as the cast does.
SELECT i, sum(i) OVER (ORDER BY i ROWS BETWEEN 1.5 PRECEDING AND CURRENT ROW) FROM wo ORDER BY i
# Negative (and NaN) offsets are 22013, each with its own message.
SELECT i, sum(i) OVER (ORDER BY i RANGE BETWEEN -1 PRECEDING AND CURRENT ROW) FROM wo
SELECT i, sum(i) OVER (ORDER BY i ROWS BETWEEN -1 PRECEDING AND CURRENT ROW) FROM wo
SELECT i, sum(i) OVER (ORDER BY i GROUPS BETWEEN CURRENT ROW AND -1 FOLLOWING) FROM wo
SELECT f, sum(i) OVER (ORDER BY f RANGE BETWEEN 'NaN' PRECEDING AND CURRENT ROW) FROM wo
DROP TABLE wo
# RANGE offsets over numeric keys past f64's precision compare exactly.
create table wr_t (x numeric)
insert into wr_t values (100000000000000000000), (100000000000000000001), (100000000000000000002), (1.5), (1.55), (null)
select x, count(*) over (order by x range between 1 preceding and current row) from wr_t order by x
select x, count(*) over (order by x desc range between 1 preceding and 1 following) from wr_t order by x desc
select x, sum(x) over (order by x range between current row and 0.05 following) from wr_t order by x
select x, count(*) over (order by x range between 1 following and 2 following) from wr_t order by x
drop table wr_t
