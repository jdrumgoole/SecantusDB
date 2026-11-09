//! The ICU 57.1 collator `mongod` 8.2 links, built from mongo's own copy of
//! the sources and data (`vendor/icu4c-57.1`, see `build.rs`).
//!
//! The surface is the one mongod uses: open a collator for a locale, read and
//! set its attributes, compare two UTF-8 strings, make a sort key.

use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::OnceLock;

/// The ICU version mongod reports as a collation's `version`.
pub const ICU_VERSION: &str = "57.1";

/// `UColAttribute` values (`unicode/ucol.h`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum Attribute {
    FrenchCollation = 0,
    AlternateHandling = 1,
    CaseFirst = 2,
    CaseLevel = 3,
    NormalizationMode = 4,
    Strength = 5,
    NumericCollation = 7,
}

/// `UColAttributeValue` values.
pub mod value {
    pub const PRIMARY: i32 = 0;
    pub const SECONDARY: i32 = 1;
    pub const TERTIARY: i32 = 2;
    pub const QUATERNARY: i32 = 3;
    pub const IDENTICAL: i32 = 15;
    pub const OFF: i32 = 16;
    pub const ON: i32 = 17;
    pub const SHIFTED: i32 = 20;
    pub const NON_IGNORABLE: i32 = 21;
    pub const LOWER_FIRST: i32 = 24;
    pub const UPPER_FIRST: i32 = 25;
}

/// `UColReorderCode` values `setMaxVariable` takes.
pub mod reorder {
    pub const SPACE: i32 = 0x1000;
    pub const PUNCTUATION: i32 = 0x1001;
}

extern "C" {
    fn secantus_icu_set_common_data(data: *const c_void) -> i32;
    fn secantus_icu_open(
        locale: *const c_char,
        out: *mut *mut c_void,
        bogus: *mut i32,
        name: *mut c_char,
        name_cap: i32,
        valid: *mut c_char,
        valid_cap: i32,
    ) -> i32;
    fn secantus_icu_close(c: *mut c_void);
    fn secantus_icu_get_attribute(c: *mut c_void, attr: i32, value: *mut i32) -> i32;
    fn secantus_icu_set_attribute(c: *mut c_void, attr: i32, value: i32) -> i32;
    fn secantus_icu_get_max_variable(c: *mut c_void) -> i32;
    fn secantus_icu_set_max_variable(c: *mut c_void, group: i32) -> i32;
    fn secantus_icu_compare(
        c: *const c_void,
        a: *const c_char,
        alen: i32,
        b: *const c_char,
        blen: i32,
        status: *mut i32,
    ) -> i32;
    fn secantus_icu_sort_key(
        c: *const c_void,
        s: *const c_char,
        len: i32,
        out: *mut u8,
        cap: i32,
        status: *mut i32,
    ) -> i32;
}

/// ICU wants its common data aligned; a `.dat` is laid out for 16.
#[repr(C, align(16))]
struct Aligned<T: ?Sized>(T);

static DATA: &Aligned<[u8]> = &Aligned(*include_bytes!(env!("SECANTUS_ICU_DAT")));

/// Hand ICU mongo's data, once, before the first collator.
fn init() -> Result<(), Error> {
    static DONE: OnceLock<i32> = OnceLock::new();
    let status =
        *DONE.get_or_init(|| unsafe { secantus_icu_set_common_data(DATA.0.as_ptr().cast()) });
    check(status)
}

/// An ICU `UErrorCode` that is a failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error(pub i32);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ICU error {}", self.0)
    }
}

impl std::error::Error for Error {}

fn check(status: i32) -> Result<(), Error> {
    // U_ZERO_ERROR is 0; warnings are negative, failures positive.
    if status > 0 {
        Err(Error(status))
    } else {
        Ok(())
    }
}

/// What opening a collator found.
pub enum Opened {
    /// `icu::Locale::createFromName` called the locale bogus.
    Bogus,
    /// A collator, with the locale's canonical name (`Locale::getName`) and
    /// the locale ICU actually has data for (`ULOC_VALID_LOCALE`).
    Collator {
        collator: Collator,
        name: String,
        valid: String,
    },
}

/// An `icu::Collator`. Settings are made before it is shared; once shared it
/// is only read (`compareUTF8`, `getCollationKey`), which ICU's
/// `RuleBasedCollator` allows from any number of threads.
pub struct Collator(*mut c_void);

// SAFETY: see the type's documentation: the `&self` methods are ICU const
// methods, thread-safe on a RuleBasedCollator; the mutating ones take `&mut`.
unsafe impl Send for Collator {}
unsafe impl Sync for Collator {}

impl Drop for Collator {
    fn drop(&mut self) {
        unsafe { secantus_icu_close(self.0) }
    }
}

impl Collator {
    /// `Locale::createFromName(locale)` then `Collator::createInstance`.
    pub fn open(locale: &str) -> Result<Opened, Error> {
        init()?;
        let c_locale = CString::new(locale).map_err(|_| Error(1))?;
        let mut out = std::ptr::null_mut();
        let mut bogus = 0;
        let mut name = [0 as c_char; 160];
        let mut valid = [0 as c_char; 160];
        let status = unsafe {
            secantus_icu_open(
                c_locale.as_ptr(),
                &mut out,
                &mut bogus,
                name.as_mut_ptr(),
                name.len() as i32,
                valid.as_mut_ptr(),
                valid.len() as i32,
            )
        };
        check(status)?;
        if bogus != 0 {
            return Ok(Opened::Bogus);
        }
        let text = |b: &[c_char]| {
            unsafe { CStr::from_ptr(b.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        };
        Ok(Opened::Collator {
            collator: Collator(out),
            name: text(&name),
            valid: text(&valid),
        })
    }

    pub fn attribute(&self, attr: Attribute) -> Result<i32, Error> {
        let mut v = 0;
        check(unsafe { secantus_icu_get_attribute(self.0, attr as i32, &mut v) })?;
        Ok(v)
    }

    pub fn set_attribute(&mut self, attr: Attribute, value: i32) -> Result<(), Error> {
        check(unsafe { secantus_icu_set_attribute(self.0, attr as i32, value) })
    }

    pub fn max_variable(&self) -> i32 {
        unsafe { secantus_icu_get_max_variable(self.0) }
    }

    pub fn set_max_variable(&mut self, group: i32) -> Result<(), Error> {
        check(unsafe { secantus_icu_set_max_variable(self.0, group) })
    }

    /// `compareUTF8` over bytes that may not be UTF-8: ICU weighs each
    /// invalid sequence as U+FFFD, as mongod's comparisons do.
    pub fn compare_bytes(&self, a: &[u8], b: &[u8]) -> std::cmp::Ordering {
        let mut status = 0;
        let r = unsafe {
            secantus_icu_compare(
                self.0,
                a.as_ptr().cast(),
                len_bytes(a),
                b.as_ptr().cast(),
                len_bytes(b),
                &mut status,
            )
        };
        if let Err(e) = check(status) {
            panic!("ICU compareUTF8 failed: {e}");
        }
        r.cmp(&0)
    }

    /// `compareUTF8`. ICU fails it only on an allocation failure, which
    /// mongod treats as fatal (fassert 34438); so does this.
    pub fn compare(&self, a: &str, b: &str) -> std::cmp::Ordering {
        self.compare_bytes(a.as_bytes(), b.as_bytes())
    }

    /// The sort key mongod's `getComparisonKey` makes: ICU's collation key
    /// without its trailing NUL. Fatal on failure, as mongod's fassert 34439.
    pub fn sort_key_into(&self, s: &str, out: &mut Vec<u8>) {
        let start = out.len();
        let mut cap = s.len() * 3 + 16;
        loop {
            out.resize(start + cap, 0);
            let mut status = 0;
            let n = unsafe {
                secantus_icu_sort_key(
                    self.0,
                    s.as_ptr().cast(),
                    len(s),
                    out[start..].as_mut_ptr(),
                    cap as i32,
                    &mut status,
                )
            };
            if let Err(e) = check(status) {
                panic!("ICU getCollationKey failed: {e}");
            }
            let n = n as usize;
            if n <= cap {
                out.truncate(start + n);
                return;
            }
            cap = n;
        }
    }

    pub fn sort_key(&self, s: &str) -> Vec<u8> {
        let mut out = Vec::new();
        self.sort_key_into(s, &mut out);
        out
    }
}

fn len(s: &str) -> i32 {
    len_bytes(s.as_bytes())
}

fn len_bytes(s: &[u8]) -> i32 {
    i32::try_from(s.len()).expect("string over 2 GiB")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(locale: &str) -> Collator {
        match Collator::open(locale).unwrap() {
            Opened::Collator { collator, .. } => collator,
            Opened::Bogus => panic!("bogus {locale}"),
        }
    }

    #[test]
    fn mongos_data_carries_the_types_icu4x_lacks() {
        for locale in [
            "en",
            "ko@collation=search",
            "ko@collation=searchjl",
            "zh@collation=big5han",
            "zh@collation=gb2312han",
            "ln@collation=phonetic",
        ] {
            match Collator::open(locale).unwrap() {
                Opened::Collator { valid, .. } => assert_eq!(valid, locale),
                Opened::Bogus => panic!("bogus {locale}"),
            }
        }
    }

    #[test]
    fn normalization_off_tells_canonical_equivalents_apart() {
        let mut c = open("en");
        assert_eq!(
            c.attribute(Attribute::NormalizationMode).unwrap(),
            value::OFF
        );
        let (a, b) = ("\u{1ECD}\u{308}", "\u{F6}\u{323}");
        assert_ne!(c.sort_key(a), c.sort_key(b));
        c.set_attribute(Attribute::NormalizationMode, value::ON)
            .unwrap();
        assert_eq!(c.compare(a, b), std::cmp::Ordering::Equal);
    }

    #[test]
    fn sort_keys_are_mongods_bytes() {
        let mut c = open("en");
        c.set_attribute(Attribute::Strength, value::SECONDARY)
            .unwrap();
        // mongod 8.2.11 under `{locale: "en", strength: 2}`: "x" is the key
        // "W\x01\x05" in a duplicate-key error, "a" is 290105.
        assert_eq!(c.sort_key("x"), [0x57, 0x01, 0x05]);
        assert_eq!(c.sort_key("a"), [0x29, 0x01, 0x05]);
        assert_eq!(c.compare("a", "A"), std::cmp::Ordering::Equal);
    }
}
