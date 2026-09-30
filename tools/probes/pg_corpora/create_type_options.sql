# reference-version: 15
# CREATE TYPE's attributes (DefineType / TypeCreate): each one's value, its
# validation and error order, and pg_type's physical and I/O columns for
# built-in, enum, composite, range, domain, shell and user base types.
create type ct_b
create function ct_b_in(cstring) returns ct_b language internal immutable strict as 'int4in'
create function ct_b_out(ct_b) returns cstring language internal immutable strict as 'int4out'
create type ct_b (input = ct_b_in, output = ct_b_out, internallength = 4, passedbyvalue, alignment = int4, storage = plain, category = 'N', preferred = false, default = '0', delimiter = ',')
select typname, typlen, typbyval, typalign, typstorage, typcategory, typispreferred, typdefault, typdelim from pg_type where typname = 'ct_b'
create type ct_c
create function ct_c_in(cstring) returns ct_c language internal immutable strict as 'textin'
create function ct_c_out(ct_c) returns cstring language internal immutable strict as 'textout'
create function ct_c_recv(internal) returns ct_c language internal immutable strict as 'textrecv'
create function ct_c_send(ct_c) returns bytea language internal immutable strict as 'textsend'
create type ct_c (input = ct_c_in, output = ct_c_out, receive = ct_c_recv, send = ct_c_send, internallength = variable, storage = extended, category = 'S', collatable = true)
select typname, typlen, typbyval, typalign, typstorage, typcategory, typcollation <> 0, typreceive::text, typsend::text from pg_type where typname = 'ct_c'
create type ct_d (input = ct_c_in, output = ct_c_out, category = 'toolong')
create type ct_d (input = ct_c_in, output = ct_c_out, alignment = weird)
create type ct_d (input = ct_c_in, output = ct_c_out, internallength = -5)
create type ct_d (input = ct_c_in, output = ct_c_out, bogus = 1)
drop type ct_b cascade
drop type ct_c cascade
select typname, typlen, typbyval, typalign, typstorage, typcategory, typispreferred, typinput, typoutput, typreceive, typsend from pg_type where typname in ('int4', 'text', 'numeric', '_int4', 'bool', 'timestamptz', 'interval') order by 1
create type pt_e as enum ('a')
create type pt_c as (a int)
create type pt_r as range (subtype = int4)
create domain pt_d as varchar(5)
select typname, typlen, typbyval, typalign, typstorage, typcategory, typinput, typoutput from pg_type where typname in ('pt_e', 'pt_c', 'pt_r', 'pt_d') order by 1
create type pt_s
select typname, typlen, typbyval, typalign, typstorage, typcategory, typisdefined, typinput, typoutput from pg_type where typname = 'pt_s'
drop type pt_s
drop domain pt_d
drop type pt_e, pt_c, pt_r
create type cto_0
create function cto_0_in(cstring) returns cto_0 language internal immutable strict as 'textin'
create function cto_0_out(cto_0) returns cstring language internal immutable strict as 'textout'
create type cto_0 (input = cto_0_in, output = cto_0_out, alignment = weird)
select typlen, typbyval, typalign, typstorage, typcategory, typispreferred, typdefault, typdelim from pg_type where typname = 'cto_0'
drop type cto_0 cascade
create type cto_1
create function cto_1_in(cstring) returns cto_1 language internal immutable strict as 'textin'
create function cto_1_out(cto_1) returns cstring language internal immutable strict as 'textout'
create type cto_1 (input = cto_1_in, output = cto_1_out, internallength = -5)
select typlen, typbyval, typalign, typstorage, typcategory, typispreferred, typdefault, typdelim from pg_type where typname = 'cto_1'
drop type cto_1 cascade
create type cto_2
create function cto_2_in(cstring) returns cto_2 language internal immutable strict as 'textin'
create function cto_2_out(cto_2) returns cstring language internal immutable strict as 'textout'
create type cto_2 (input = cto_2_in, output = cto_2_out, storage = odd)
select typlen, typbyval, typalign, typstorage, typcategory, typispreferred, typdefault, typdelim from pg_type where typname = 'cto_2'
drop type cto_2 cascade
create type cto_3
create function cto_3_in(cstring) returns cto_3 language internal immutable strict as 'textin'
create function cto_3_out(cto_3) returns cstring language internal immutable strict as 'textout'
create type cto_3 (input = cto_3_in, output = cto_3_out, internallength = 4, passedbyvalue, alignment = double)
select typlen, typbyval, typalign, typstorage, typcategory, typispreferred, typdefault, typdelim from pg_type where typname = 'cto_3'
drop type cto_3 cascade
create type cto_4
create function cto_4_in(cstring) returns cto_4 language internal immutable strict as 'textin'
create function cto_4_out(cto_4) returns cstring language internal immutable strict as 'textout'
create type cto_4 (input = cto_4_in, output = cto_4_out, bogus = 1)
select typlen, typbyval, typalign, typstorage, typcategory, typispreferred, typdefault, typdelim from pg_type where typname = 'cto_4'
drop type cto_4 cascade
create type cto_5
create function cto_5_in(cstring) returns cto_5 language internal immutable strict as 'textin'
create function cto_5_out(cto_5) returns cstring language internal immutable strict as 'textout'
create type cto_5 (input = cto_5_in, output = cto_5_out, internallength = 3, passedbyvalue)
select typlen, typbyval, typalign, typstorage, typcategory, typispreferred, typdefault, typdelim from pg_type where typname = 'cto_5'
drop type cto_5 cascade
create type cto_6
create function cto_6_in(cstring) returns cto_6 language internal immutable strict as 'textin'
create function cto_6_out(cto_6) returns cstring language internal immutable strict as 'textout'
create type cto_6 (input = cto_6_in, output = cto_6_out, passedbyvalue)
select typlen, typbyval, typalign, typstorage, typcategory, typispreferred, typdefault, typdelim from pg_type where typname = 'cto_6'
drop type cto_6 cascade
create type cto_7
create function cto_7_in(cstring) returns cto_7 language internal immutable strict as 'textin'
create function cto_7_out(cto_7) returns cstring language internal immutable strict as 'textout'
create type cto_7 (input = cto_7_in, output = cto_7_out, category = '')
select typlen, typbyval, typalign, typstorage, typcategory, typispreferred, typdefault, typdelim from pg_type where typname = 'cto_7'
drop type cto_7 cascade
create type cto_8
create function cto_8_in(cstring) returns cto_8 language internal immutable strict as 'textin'
create function cto_8_out(cto_8) returns cstring language internal immutable strict as 'textout'
create type cto_8 (input = cto_8_in, output = cto_8_out, delimiter = 'ab')
select typlen, typbyval, typalign, typstorage, typcategory, typispreferred, typdefault, typdelim from pg_type where typname = 'cto_8'
drop type cto_8 cascade
create type cto_9
create function cto_9_in(cstring) returns cto_9 language internal immutable strict as 'textin'
create function cto_9_out(cto_9) returns cstring language internal immutable strict as 'textout'
create type cto_9 (input = cto_9_in, output = cto_9_out, element = int4, delimiter = ';')
select typlen, typbyval, typalign, typstorage, typcategory, typispreferred, typdefault, typdelim from pg_type where typname = 'cto_9'
drop type cto_9 cascade
create type cto_10
create function cto_10_in(cstring) returns cto_10 language internal immutable strict as 'textin'
create function cto_10_out(cto_10) returns cstring language internal immutable strict as 'textout'
create type cto_10 (input = cto_10_in, output = cto_10_out, like = int4)
select typlen, typbyval, typalign, typstorage, typcategory, typispreferred, typdefault, typdelim from pg_type where typname = 'cto_10'
drop type cto_10 cascade
create type cto_11
create function cto_11_in(cstring) returns cto_11 language internal immutable strict as 'textin'
create function cto_11_out(cto_11) returns cstring language internal immutable strict as 'textout'
create type cto_11 (input = cto_11_in, output = cto_11_out, internallength = 8, passedbyvalue, alignment = double, storage = plain)
select typlen, typbyval, typalign, typstorage, typcategory, typispreferred, typdefault, typdelim from pg_type where typname = 'cto_11'
drop type cto_11 cascade
create type cto_12
create function cto_12_in(cstring) returns cto_12 language internal immutable strict as 'textin'
create function cto_12_out(cto_12) returns cstring language internal immutable strict as 'textout'
create type cto_12 (input = cto_12_in, output = cto_12_out, storage = main)
select typlen, typbyval, typalign, typstorage, typcategory, typispreferred, typdefault, typdelim from pg_type where typname = 'cto_12'
drop type cto_12 cascade
create type cto_13
create function cto_13_in(cstring) returns cto_13 language internal immutable strict as 'textin'
create function cto_13_out(cto_13) returns cstring language internal immutable strict as 'textout'
create type cto_13 (input = cto_13_in, output = cto_13_out, preferred = true, category = 'U')
select typlen, typbyval, typalign, typstorage, typcategory, typispreferred, typdefault, typdelim from pg_type where typname = 'cto_13'
drop type cto_13 cascade
create type cto_14
create function cto_14_in(cstring) returns cto_14 language internal immutable strict as 'textin'
create function cto_14_out(cto_14) returns cstring language internal immutable strict as 'textout'
create type cto_14 (input = cto_14_in, output = cto_14_out, default = 'x')
select typlen, typbyval, typalign, typstorage, typcategory, typispreferred, typdefault, typdelim from pg_type where typname = 'cto_14'
drop type cto_14 cascade
create type cto_15
create function cto_15_in(cstring) returns cto_15 language internal immutable strict as 'textin'
create function cto_15_out(cto_15) returns cstring language internal immutable strict as 'textout'
create type cto_15 (input = cto_15_in, output = cto_15_out, internallength = variable, alignment = char)
select typlen, typbyval, typalign, typstorage, typcategory, typispreferred, typdefault, typdelim from pg_type where typname = 'cto_15'
drop type cto_15 cascade
create type cto_16
create function cto_16_in(cstring) returns cto_16 language internal immutable strict as 'textin'
create function cto_16_out(cto_16) returns cstring language internal immutable strict as 'textout'
create type cto_16 (input = cto_16_in, output = cto_16_out, typmod_in = foo)
select typlen, typbyval, typalign, typstorage, typcategory, typispreferred, typdefault, typdelim from pg_type where typname = 'cto_16'
drop type cto_16 cascade
create type cto_17
create function cto_17_in(cstring) returns cto_17 language internal immutable strict as 'textin'
create function cto_17_out(cto_17) returns cstring language internal immutable strict as 'textout'
create type cto_17 (input = cto_17_in, output = cto_17_out, internallength = 4, storage = main)
select typlen, typbyval, typalign, typstorage, typcategory, typispreferred, typdefault, typdelim from pg_type where typname = 'cto_17'
drop type cto_17 cascade
