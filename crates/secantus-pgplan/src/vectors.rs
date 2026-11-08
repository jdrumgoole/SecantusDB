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
}
