#!/usr/bin/env bash
# Regenerate the Rust PostgreSQL server's extra ICU collation data,
# `crates/secantus-pgplan/src/icu_baked/`.
#
#     scripts/icu_collation_data.sh
#
# ICU4X's compiled collation data has no tailoring for these languages, so
# PostgreSQL's `haw-x-icu`, `kl-x-icu`, ... ordered as the root here.
# `icu4x-datagen` bakes their data from ICU's own release (the tailorings
# ICU4C, and so PostgreSQL, has) as Rust source; `secantus-pgplan`'s
# `icu_data` module serves it to the collator for these languages only.
# Joe, 2026-10-16: generate it, the full set (the `und-Han` orderings that
# come with it are kept).
#
# The datagen version is pinned: `latest` below then names the ICU and CLDR
# releases that version was verified with, so the output is reproducible. It
# should track the `icu_collator` the crate locks (2.3 as of 2026-10-16);
# after changing either, re-run the collation probes against PostgreSQL 15
# before listing anything new in `pg_icu_collations.tsv`.
#
# Measured 2026-10-16 against PostgreSQL 15.19: these seven order as
# PostgreSQL does, with every regional form. `mr`, `as` and `kok` do NOT,
# with this data or without: ICU4X does not apply their conjunct rule
# (`&ह<क्ष`), so generating them changes nothing and they stay refused.
set -euo pipefail

DATAGEN_VERSION=2.3.0
LANGUAGES=(haw kl lkt ln se smn yi)

repo=$(cd "$(dirname "$0")/.." && pwd)
out="$repo/crates/secantus-pgplan/src/icu_baked"
tool=$(mktemp -d)
trap 'rm -rf "$tool"' EXIT

CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-3} cargo install icu4x-datagen \
    --version "=$DATAGEN_VERSION" --root "$tool" --locked --quiet

"$tool/bin/icu4x-datagen" --format baked --use-separate-crates --overwrite \
    --cldr-tag latest --icuexport-tag latest \
    --markers CollationSpecialPrimariesV1 CollationRootV1 CollationTailoringV1 \
        CollationDiacriticsV1 CollationJamoV1 CollationMetadataV1 \
        CollationReorderingV1 NormalizerNfdDataV1 NormalizerNfdTablesV1 \
    --locales "${LANGUAGES[@]}" \
    --out "$out"

echo "wrote $out"
