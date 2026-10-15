# reference-version: 15
# PostgreSQL 15's ICU collations in pg_collation (RP-005), and a COLLATE naming none is 42704 at the clause.
SELECT collname, collnamespace, collprovider, collisdeterministic, collencoding, collcollate, collctype, colliculocale FROM pg_collation WHERE collname IN ('und-x-icu', 'en-x-icu', 'de-x-icu', 'de-AT-x-icu', 'sv-x-icu', 'fr-FR-x-icu', 'tr-x-icu', 'ja-x-icu', 'zh-Hant-TW-x-icu', 'es-x-icu') ORDER BY oid
SELECT oid FROM pg_collation WHERE collname IN ('und-x-icu', 'en-x-icu', 'de-x-icu', 'sv-x-icu', 'zh-Hant-TW-x-icu') ORDER BY oid
SELECT 'de-x-icu'::regcollation::oid, 'sv-x-icu'::regcollation, to_regcollation('"ja-x-icu"')::oid
SELECT string_agg(w, ',' ORDER BY w COLLATE "sv-x-icu", w COLLATE "C") FROM icu_w
SELECT string_agg(w, ',' ORDER BY w COLLATE "de-x-icu", w COLLATE "C") FROM icu_w
SELECT string_agg(w, ',' ORDER BY w COLLATE "es-x-icu", w COLLATE "C") FROM icu_w
SELECT string_agg(w, ',' ORDER BY w COLLATE "tr-x-icu", w COLLATE "C") FROM icu_w
SELECT string_agg(w, ',' ORDER BY w COLLATE "da-x-icu", w COLLATE "C") FROM icu_w
SELECT string_agg(w, ',' ORDER BY w COLLATE "cs-x-icu", w COLLATE "C") FROM icu_w
SELECT w FROM icu_w ORDER BY w COLLATE "fi-FI-x-icu", w COLLATE "C"
SELECT string_agg(w, ',' ORDER BY w COLLATE "nope") FROM icu_w
SELECT string_agg(w, ',' ORDER BY w COLLATE "xx-x-icu") FROM icu_w
SELECT w FROM icu_w ORDER BY w COLLATE "nope"
SELECT 'a' < 'b' COLLATE "nope"
SELECT upper(w COLLATE "nope") FROM icu_w
