# reference-version: 15
# Multirange -|- << >> &< &> and the ordering comparisons, with ranges on either side (RP-015).
SELECT '{[1,3)}'::int4multirange -|- '{[3,5)}'::int4multirange, '{[1,3)}'::int4multirange << '{[5,7)}'::int4multirange, '{[1,3)}'::int4multirange >> '{[5,7)}'::int4multirange
SELECT '{[1,3)}'::int4multirange &< '{[5,7)}'::int4multirange, '{[1,3)}'::int4multirange &> '{[0,7)}'::int4multirange, '{[1,3),[8,9)}'::int4multirange &< '{[5,7)}'::int4multirange
SELECT '{[1,3)}'::int4multirange < '{[5,7)}'::int4multirange, '{[1,3)}'::int4multirange >= '{[5,7)}'::int4multirange, '{[1,3)}'::int4multirange < '{[1,3),[4,5)}'::int4multirange, '{}'::int4multirange < '{[1,2)}'::int4multirange
SELECT '{}'::int4multirange << '{[5,7)}'::int4multirange, '{[1,3),[10,12)}'::int4multirange -|- '{[12,15)}'::int4multirange, '{[5,6)}'::int4multirange -|- '{[1,3),[6,7)}'::int4multirange
SELECT '[1,3)'::int4range << '{[5,7)}'::int4multirange, '{[5,7)}'::int4multirange >> '[1,3)'::int4range, '[1,3)'::int4range -|- '{[3,4)}'::int4multirange, '{[1,3)}'::int4multirange &< '[2,9)'::int4range
SELECT '{[1.5,3)}'::nummultirange < '{[1.5,4)}'::nummultirange, '{[2020-01-01,2020-02-01)}'::datemultirange << '{[2020-03-01,2020-04-01)}'::datemultirange
