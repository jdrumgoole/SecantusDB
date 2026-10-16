//! Collation data for the locales ICU4X's compiled data does not tailor.
//!
//! PostgreSQL's ICU (ICU4C) carries tailorings for Hawaiian, Greenlandic,
//! Lakota, Lingala, Northern and Inari Sami and Yiddish that ICU4X's
//! `icu_collator` compiled data leaves out (or, for Lingala, has
//! differently), so those collations ordered as the root. `src/icu_baked/` is `icu4x-datagen`'s
//! baked output for them, from ICU's `icuexport` release; regenerate it with
//! `scripts/icu_collation_data.sh` (Joe, 2026-10-16: generated, full set).

use icu_collator::options::CollatorOptions;
use icu_collator::{Collator, CollatorPreferences};

/// The baked provider over `src/icu_baked/`.
pub(crate) struct ExtraCollationData;

// Generated code: not every constant it defines is reached.
#[allow(dead_code, clippy::all, clippy::pedantic)]
mod baked {
    use super::ExtraCollationData;
    include!("icu_baked/mod.rs");
    impl_data_provider!(ExtraCollationData);
}

/// The languages this data is for. Every other locale keeps the compiled
/// data its listing was checked with.
const LANGUAGES: [&str; 7] = ["haw", "kl", "lkt", "ln", "se", "smn", "yi"];

/// Is `language` one of the locales whose collator comes from this data?
pub(crate) fn covers(language: &str) -> bool {
    LANGUAGES.contains(&language)
}

/// A collator for `prefs` over this data.
pub(crate) fn collator(prefs: CollatorPreferences, options: CollatorOptions) -> Option<Collator> {
    Collator::try_new_unstable(&ExtraCollationData, prefs, options).ok()
}
