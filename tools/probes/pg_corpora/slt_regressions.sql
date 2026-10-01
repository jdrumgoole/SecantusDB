# Rust-server bugs the sqllogictest gauge found when first run against the
# Rust PG server (batch 32), pinned against PostgreSQL.
# BETWEEN in every shape the per-row evaluator now handles.
select count(*) from sr32_t where not null between 66 and + + a;
select count(*) from sr32_t where not - a + + c not between null and null;
select count(*) from sr32_t where null between null and - b;
select count(*) from sr32_t where a between symmetric 7 and 0;
select count(*) from sr32_t where a not between symmetric 7 and 0;
select a between b - 1 and c, 5 between symmetric 9 and 1, null not between 1 and 2 from sr32_t order by a, b;
select count(*) from sr32_t where 92 not between 8 and c;
select count(*) from sr32_t where b not between null and 3;
select count(*) from sr32_t where b not between 3 and null;
select count(*) from sr32_t where b between null and 30;
# DISTINCT over aggregates / grouped expressions, and * with GROUP BY.
select distinct + 77 as col1, count ( * ) - 11 from sr32_t;
select distinct - ( + b ) + - 51 as col0 from sr32_t group by b order by 1;
select * from sr32_t group by a, b, c, d, e order by 1, 2;
select distinct * from sr32_t group by a, b, c, d, e order by 1, 2;
select * from sr32_t group by a order by 1;
# A correlated subquery whose inner FROM aliases the outer table's name.
select a, (select count(*) from sr32_t as x where x.b < sr32_t.b) from sr32_t order by a, b;
select a from sr32_t where exists (select 1 from sr32_t as x where x.b < sr32_t.b) order by a;
select (select count(*) from sr32_t as x where sr32_t.b > 0);
# Aggregates nested in CASE / COALESCE / IN lists, and arithmetic over avg.
select all 74 * - coalesce ( + case - case when not ( not - 79 >= null ) then 48 end when + + count ( * ) then 6 end, min ( all + - 30 ) * 45 * 77 ) * - 14;
select - 39 * - + ( - nullif ( + ( + + 26 ), + case when null in ( cast ( count ( * ) as integer ), - 38 ) then + + 2 * - count ( * ) else null end * 1 ) ) as col0;
select case when 4 in (count(*), 5) then 'y' else 'n' end from sr32_t;
select - avg(cast(null as integer));
select - avg(23);
select 40 + avg(2) / count(*);
select a, avg(b) * 2 from sr32_t group by a order by 1;
# A per-row WHERE runs before DISTINCT and windows (it ran after DISTINCT,
# dropping a group's first row and every equal row behind it).
select distinct - ( 28 ) from sr32_t where ( + 92 ) not between + ( + 8 ) and ( + c );
select distinct a % 2 from sr32_t where 92 not between 8 and c order by 1;
select a, b, row_number() over (order by a, b) from sr32_t where 92 not between 8 and c order by 1, 2;
select a, count(*) over () from sr32_t where a + 0 > 2 order by 1;
