# reference-version: 15
# DROP TYPE over columns, an array column, a domain and its columns and a composite field: 2BP01 naming them in PostgreSQL's order, CASCADE dropping them with one notice; DROP DOMAIN CASCADE over two columns of one table (RP-041).
drop type dtd_e
drop domain dtd_d2
drop domain dtd_d2 cascade
select * from dtd_v
drop type dtd_e cascade
select * from dtd_t
select * from dtd_u
select '(5)'::dtd_c::text
