# secantus-icu

The ICU 57.1 collator `mongod` 8.2 links, for SecantusDB's MongoDB server.

It compiles mongo's own copy of ICU (`mongodb/mongo` r8.2.11,
`src/third_party/icu4c-57.1`, vendored at `vendor/icu4c-57.1`), with the
source lists, defines and warning switches of mongo's `BUILD.bazel`, and embeds
mongo's trimmed ICU data (`icudt57l.dat`), which it hands to
`udata_setCommonData` as mongod does. Collation answers, sort-key bytes
included, are therefore mongod's.

Internal to SecantusDB: no semver promise. ICU is under the ICU license
(`icu4c-57.1/LICENSE`).
