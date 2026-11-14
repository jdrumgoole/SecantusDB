//! An `int2vector` / `oidvector` as an ELEMENT of an array.
//!
//! A vector value is carried as an int array (`pg_index.indkey` is `[1, 2]`),
//! which is what subscripting it from 0 and `= any(indkey)` read. Inside an
//! array that form is ambiguous: the array machinery reads a nested
//! `Bson::Array` as another dimension, so `array['1 2'::int2vector]` was the
//! two-dimensional `{{1,2}}`, `array['1 2'::oidvector, '3']` was refused as
//! ragged, and `'{"1 2"}'::int2vector[]` read its element as a dimension.
//!
//! On PostgreSQL 15.19 the element is ONE datum, opaque to `array_in` /
//! `array_out`, which print it by `int2vectorout` (`{"1 2"}`, `{""}` for an
//! empty vector). So inside an array a vector is carried as that text, its
//! canonical `int2vectorout` / `oidvectorout` form: the dimension, length,
//! subscript and unnest machinery then sees one element, and the text array
//! printer quotes it as `array_out` does. Taken back out of the array (a
//! subscript, `unnest`), [`scalar`] turns it into the int array again.

use crate::Bson;

/// Is `ty` one of the two vector types?
pub fn is_vector(ty: &str) -> bool {
    matches!(ty.trim(), "int2vector" | "oidvector")
}

/// `check_valid_int2vector` / `check_valid_oidvector`: a vector's output
/// function refuses anything but one dimension, so `array_agg` over vectors
/// (typed as the vector, holding two) cannot be printed (42804).
pub fn check_valid(v: &Bson, ty: &str) -> crate::Result<()> {
    let ty = ty.trim();
    match v {
        Bson::Array(items)
            if is_vector(ty) && items.iter().any(|x| matches!(x, Bson::Array(_))) =>
        {
            Err(crate::Error::DatatypeMismatch(format!(
                "array is not a valid {ty}"
            )))
        }
        _ => Ok(()),
    }
}

/// The element type of `ty` when it is an array of a vector type.
pub fn vector_array_element(ty: &str) -> Option<&str> {
    let elem = ty.trim().strip_suffix("[]")?;
    let elem = elem.trim_end_matches("[]");
    is_vector(elem).then_some(elem)
}

/// Is `v` a vector in its scalar form -- an int array, which no dimension of
/// a vector array is (a dimension holds the text form, or more dimensions)?
fn is_scalar_form(items: &[Bson]) -> bool {
    items
        .iter()
        .all(|x| !matches!(x, Bson::Array(_) | Bson::String(_) | Bson::Null))
}

/// `int2vectorout` / `oidvectorout` of a vector's int list.
fn text_of(items: &[Bson]) -> String {
    items
        .iter()
        .map(crate::value_text)
        .collect::<Vec<_>>()
        .join(" ")
}

/// One vector as an array element: its canonical text. A text element is
/// read by the vector's input function first, so its errors are the type's
/// own (`'{"1 x"}'::int2vector[]` is 22P02 for `smallint`) and its spacing
/// is `int2vectorout`'s.
pub fn element(v: Bson, elem: &str) -> crate::Result<Bson> {
    match v {
        Bson::Array(items) => Ok(Bson::String(text_of(&items))),
        Bson::String(_) => match crate::cast_value(v, elem)? {
            Bson::Array(items) => Ok(Bson::String(text_of(&items))),
            other => Ok(other),
        },
        other => Ok(other),
    }
}

/// C's `isspace` in the "C" locale: what `int2vectorin` / `oidvectorin`
/// skip between numbers. Rust's ASCII whitespace lacks the vertical tab.
fn c_isspace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `strtol` / `strtoul` over `s` (whitespace already skipped): an optional
/// sign, then decimal digits. Returns whether it is negative, the magnitude
/// (`None` when it overflows 64 bits: `ERANGE`), and the bytes consumed --
/// 0 when no digit followed, as `endptr == s` is in C.
fn strto_digits(s: &[u8]) -> (bool, Option<u64>, usize) {
    let mut i = 0;
    let negative = match s.first() {
        Some(b'-') => {
            i = 1;
            true
        }
        Some(b'+') => {
            i = 1;
            false
        }
        _ => false,
    };
    let start = i;
    let mut magnitude: Option<u64> = Some(0);
    while let Some(d) = s.get(i).filter(|b| b.is_ascii_digit()) {
        magnitude = magnitude
            .and_then(|m| m.checked_mul(10))
            .and_then(|m| m.checked_add(u64::from(d - b'0')));
        i += 1;
    }
    if i == start {
        return (false, Some(0), 0);
    }
    (negative, magnitude, i)
}

/// The rest of the input from `at`, as the C functions quote their
/// `char *` cursor in a message.
fn rest(text: &str, at: usize) -> &str {
    text.get(at..).unwrap_or("")
}

/// `int2vectorin` (PostgreSQL 15.19, `int.c`): numbers separated by
/// whitespace, each read by `strtol`. Every error quotes the input from the
/// failing number to the end (`value "70000 1" is out of range for type
/// smallint`), and a number must be followed by a space or the end, so
/// `'1x 2'` and a tab after a number are syntax errors.
pub fn int2vector_in(text: &str) -> crate::Result<Vec<Bson>> {
    let bytes = text.as_bytes();
    let mut at = 0;
    let mut out = Vec::new();
    loop {
        while bytes.get(at).is_some_and(|b| c_isspace(*b)) {
            at += 1;
        }
        if at >= bytes.len() {
            return Ok(out);
        }
        let (negative, magnitude, len) = strto_digits(&bytes[at..]);
        if len == 0 {
            return Err(crate::Error::InvalidText(format!(
                "invalid input syntax for type smallint: \"{}\"",
                rest(text, at)
            )));
        }
        let value = magnitude
            .and_then(|m| i64::try_from(m).ok())
            .map(|m| if negative { -m } else { m })
            .and_then(|v| i16::try_from(v).ok());
        let Some(value) = value else {
            return Err(crate::Error::NumericOutOfRange(format!(
                "value \"{}\" is out of range for type smallint",
                rest(text, at)
            )));
        };
        if bytes.get(at + len).is_some_and(|b| *b != b' ') {
            return Err(crate::Error::InvalidText(format!(
                "invalid input syntax for type smallint: \"{}\"",
                rest(text, at)
            )));
        }
        out.push(Bson::Int32(i32::from(value)));
        at += len;
    }
}

/// `oidvectorin` (PostgreSQL 15.19, `oid.c`): numbers separated by
/// whitespace, each read by `oidin_subr`, which takes `strtoul`'s answer --
/// so `'-1'` is 4294967295 -- and leaves the byte after the number to the
/// next round: `'1-2'` is `1 4294967294`, and `'1x'` fails on `"x"`.
pub fn oidvector_in(text: &str) -> crate::Result<Vec<Bson>> {
    let bytes = text.as_bytes();
    let mut at = 0;
    let mut out = Vec::new();
    loop {
        while bytes.get(at).is_some_and(|b| c_isspace(*b)) {
            at += 1;
        }
        if at >= bytes.len() {
            return Ok(out);
        }
        let (negative, magnitude, len) = strto_digits(&bytes[at..]);
        if len == 0 {
            return Err(crate::Error::InvalidText(format!(
                "invalid input syntax for type oid: \"{}\"",
                rest(text, at)
            )));
        }
        // `strtoul` negates a `-` magnitude as an unsigned long; then the
        // value is an oid when it fits 32 bits, or is a sign-extended
        // 32-bit int (`cvt == (unsigned long) (int) result`).
        let oid = magnitude
            .map(|m| if negative { m.wrapping_neg() } else { m })
            .and_then(|cvt| {
                let result = cvt as u32;
                let sign_extended = i64::from(result as i32) as u64;
                (cvt == u64::from(result) || cvt == sign_extended).then_some(result)
            });
        let Some(oid) = oid else {
            return Err(crate::Error::NumericOutOfRange(format!(
                "value \"{}\" is out of range for type oid",
                rest(text, at)
            )));
        };
        out.push(Bson::Int64(i64::from(oid)));
        at += len;
    }
}

/// Every element of an array of vectors in its element form, walking the
/// dimensions: an int array at an element's place is one vector, and an
/// array of anything else (texts, NULLs, more arrays) is a dimension.
pub fn elements(v: Bson, elem: &str) -> crate::Result<Bson> {
    // An array with lower bounds of its own (`'[0:0]={"1 2"}'`) keeps them.
    if crate::arrays::is_bounded(&v) {
        let lower = crate::arrays::lower_bounds(&v);
        let items = elements(crate::arrays::strip(&v), elem)?;
        return Ok(crate::arrays::bounded(items, &lower));
    }
    match v {
        Bson::Array(items) if is_scalar_form(&items) && !items.is_empty() => {
            // A lone vector where an array of them was expected is a
            // one-dimensional array of ints, which only the caller's own
            // shape rules can read: leave it as it stands.
            Ok(Bson::Array(items))
        }
        Bson::Array(items) => items
            .into_iter()
            .map(|x| match x {
                Bson::Array(inner) if is_scalar_form(&inner) => element(Bson::Array(inner), elem),
                Bson::Array(inner) => elements(Bson::Array(inner), elem),
                other => element(other, elem),
            })
            .collect::<crate::Result<Vec<_>>>()
            .map(Bson::Array),
        other => Ok(other),
    }
}

/// A vector taken out of an array (a subscript, `unnest`): its scalar int
/// array again. Anything not in the element form is returned unchanged.
pub fn scalar(v: Bson, elem: &str) -> Bson {
    match v {
        Bson::String(_) => crate::cast_value(v.clone(), elem).unwrap_or(v),
        other => other,
    }
}

/// Every element of an array of vectors in its scalar form, dimensions kept:
/// what a comparison with each element (`= ANY`, `array_position`) reads.
pub fn scalars(v: Bson, elem: &str) -> Bson {
    match v {
        Bson::Array(items) => Bson::Array(
            items
                .into_iter()
                .map(|x| match x {
                    Bson::String(_) => scalar(x, elem),
                    Bson::Array(inner) if !is_scalar_form(&inner) => {
                        scalars(Bson::Array(inner), elem)
                    }
                    other => other,
                })
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ints(v: &[i32]) -> Bson {
        Bson::Array(v.iter().map(|i| Bson::Int32(*i)).collect())
    }

    fn text(s: &str) -> Bson {
        Bson::String(s.into())
    }

    #[test]
    fn each_vector_is_one_element() {
        // `array['1 2'::int2vector, '3']`: two vectors of different lengths.
        let got = elements(Bson::Array(vec![ints(&[1, 2]), ints(&[3])]), "int2vector").unwrap();
        assert_eq!(got, Bson::Array(vec![text("1 2"), text("3")]));
        // An empty vector is the empty text, and a NULL stays one.
        let got = elements(Bson::Array(vec![ints(&[]), Bson::Null]), "oidvector").unwrap();
        assert_eq!(got, Bson::Array(vec![text(""), Bson::Null]));
    }

    #[test]
    fn dimensions_are_walked() {
        // `{{"1 2"},{"3"}}`: a dimension holds vectors, in either form.
        let got = elements(
            Bson::Array(vec![
                Bson::Array(vec![ints(&[1, 2])]),
                Bson::Array(vec![text("3")]),
            ]),
            "int2vector",
        )
        .unwrap();
        assert_eq!(
            got,
            Bson::Array(vec![
                Bson::Array(vec![text("1 2")]),
                Bson::Array(vec![text("3")]),
            ])
        );
    }

    #[test]
    fn text_is_read_by_the_input_function() {
        // `int2vectorout` spacing, and `int2vectorin`'s own error.
        assert_eq!(
            element(text(" 01   2 "), "int2vector").unwrap(),
            text("1 2")
        );
        assert!(element(text("1 x"), "int2vector").is_err());
    }

    fn err(r: crate::Result<Vec<Bson>>) -> String {
        match r {
            Err(crate::Error::InvalidText(m)) => format!("22P02 {m}"),
            Err(crate::Error::NumericOutOfRange(m)) => format!("22003 {m}"),
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn int2vectorin_quotes_the_rest_of_the_input() {
        // Each answer measured on PostgreSQL 15.19 (RP-605).
        let range = "22003 value \"70000 1\" is out of range for type smallint";
        assert_eq!(err(int2vector_in("70000 1")), range);
        assert_eq!(err(int2vector_in("1 70000 1")), range);
        assert_eq!(
            err(int2vector_in("99999999999999999999 1")),
            "22003 value \"99999999999999999999 1\" is out of range for type smallint"
        );
        assert_eq!(
            err(int2vector_in("-32769")),
            "22003 value \"-32769\" is out of range for type smallint"
        );
        let syntax = |s: &str| format!("22P02 invalid input syntax for type smallint: \"{s}\"");
        assert_eq!(err(int2vector_in("1 x 2")), syntax("x 2"));
        assert_eq!(err(int2vector_in("1x 2")), syntax("1x 2"));
        assert_eq!(err(int2vector_in("1\t2")), syntax("1\t2"));
        assert_eq!(err(int2vector_in(" 1\t")), syntax("1\t"));
        assert_eq!(err(int2vector_in("\t1  2\n")), syntax("2\n"));
        assert_eq!(err(int2vector_in("1 -")), syntax("-"));
        assert_eq!(err(int2vector_in("1,2")), syntax("1,2"));
        assert_eq!(err(int2vector_in("0x1")), syntax("0x1"));
        assert_eq!(Bson::Array(int2vector_in("+5 -3").unwrap()), ints(&[5, -3]));
        assert_eq!(
            Bson::Array(int2vector_in("-32768 32767").unwrap()),
            ints(&[-32768, 32767])
        );
        assert_eq!(int2vector_in("  ").unwrap(), Vec::<Bson>::new());
    }

    #[test]
    fn oidvectorin_reads_each_number_by_strtoul() {
        // Each answer measured on PostgreSQL 15.19.
        let oids = |v: &[i64]| v.iter().map(|o| Bson::Int64(*o)).collect::<Vec<_>>();
        assert_eq!(oidvector_in("1-2").unwrap(), oids(&[1, 4294967294]));
        assert_eq!(oidvector_in("-1").unwrap(), oids(&[4294967295]));
        assert_eq!(oidvector_in("1\n2").unwrap(), oids(&[1, 2]));
        assert_eq!(oidvector_in("").unwrap(), oids(&[]));
        let syntax = |s: &str| format!("22P02 invalid input syntax for type oid: \"{s}\"");
        assert_eq!(err(oidvector_in("1x")), syntax("x"));
        assert_eq!(err(oidvector_in("x 1")), syntax("x 1"));
        let range = |s: &str| format!("22003 value \"{s}\" is out of range for type oid");
        assert_eq!(err(oidvector_in("99999999999 1")), range("99999999999 1"));
        assert_eq!(err(oidvector_in("4294967296")), range("4294967296"));
        assert_eq!(
            err(oidvector_in("-2147483648 -2147483649")),
            range("-2147483649")
        );
    }

    #[test]
    fn out_of_the_array_it_is_an_int_array_again() {
        assert_eq!(scalar(text("1 2"), "int2vector"), ints(&[1, 2]));
        assert_eq!(
            scalars(Bson::Array(vec![text("1 2"), Bson::Null]), "int2vector"),
            Bson::Array(vec![ints(&[1, 2]), Bson::Null])
        );
        assert_eq!(vector_array_element("int2vector[]"), Some("int2vector"));
        assert_eq!(vector_array_element("int2vector"), None);
        assert_eq!(vector_array_element("int4[]"), None);
    }

    #[test]
    fn a_two_dimensional_vector_has_no_text() {
        let two = Bson::Array(vec![ints(&[1, 2])]);
        let err = check_valid(&two, "int2vector").unwrap_err();
        assert!(
            matches!(err, crate::Error::DatatypeMismatch(m) if m == "array is not a valid int2vector")
        );
        assert!(check_valid(&ints(&[1, 2]), "oidvector").is_ok());
        assert!(check_valid(&two, "int2[]").is_ok());
    }
}
