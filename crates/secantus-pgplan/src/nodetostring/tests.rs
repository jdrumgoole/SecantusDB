//! The printer and the datum encoders against text PostgreSQL 15.19 wrote.
//! Each expected string was read from `pg_rewrite.ev_action` of a view
//! created on a PostgreSQL 15.19 cluster (inside a rolled-back
//! transaction).

use super::datum::{interval_in_typmod, numeric_in};
use super::nodes::*;
use super::out::out_token;
use super::{node_to_string, queries_to_string};

fn konst(consttype: u32, len: i32, byval: bool, coll: u32, value: Datum, location: i32) -> Node {
    Node::Const(Const {
        consttype,
        consttypmod: -1,
        constcollid: coll,
        constlen: len,
        constbyval: byval,
        constvalue: Some(value),
        location,
    })
}

#[test]
fn int4_const_prints_its_eight_datum_bytes() {
    let value = Datum::ByVal {
        bytes: 1i64.to_le_bytes(),
        len: 4,
    };
    let n = konst(23, 4, true, 0, value, 25);
    assert_eq!(
        node_to_string(Some(&n)),
        "{CONST :consttype 23 :consttypmod -1 :constcollid 0 :constlen 4 :constbyval true \
         :constisnull false :location 25 :constvalue 4 [ 1 0 0 0 0 0 0 0 ]}"
    );
}

#[test]
fn numeric_literal_is_the_short_varlena_with_signed_bytes() {
    let n = konst(1700, -1, false, 0, numeric_in("1.50", -1).unwrap(), 33);
    assert_eq!(
        node_to_string(Some(&n)),
        "{CONST :consttype 1700 :consttypmod -1 :constcollid 0 :constlen -1 :constbyval false \
         :constisnull false :location 33 :constvalue 10 [ 40 0 0 0 0 -127 1 0 -120 19 ]}"
    );
    // `-12345.678::numeric` is `-(12345.678::numeric)`: the cast binds
    // tighter than the unary minus, so the constant is positive.
    let n = konst(1700, -1, false, 0, numeric_in("12345.678", -1).unwrap(), 96);
    assert!(node_to_string(Some(&n))
        .ends_with(":constvalue 12 [ 48 0 0 0 -127 -127 1 0 41 9 124 26 ]}"));
}

#[test]
fn interval_literal_is_the_sixteen_byte_struct() {
    let n = konst(
        1186,
        16,
        false,
        0,
        interval_in_typmod("1 day 02:00:03", -1).unwrap(),
        53,
    );
    assert!(node_to_string(Some(&n))
        .ends_with(":location 53 :constvalue 16 [ -64 14 85 -83 1 0 0 0 1 0 0 0 0 0 0 0 ]}"));
}

#[test]
fn text_const_carries_its_collation_and_varlena_header() {
    let value = Datum::ByRef(vec![28, 0, 0, 0, b'a', b' ', b'b']);
    let n = konst(25, -1, false, 100, value, 77);
    assert!(node_to_string(Some(&n)).ends_with(
        ":constcollid 100 :constlen -1 :constbyval false :constisnull false \
         :location 77 :constvalue 7 [ 28 0 0 0 97 32 98 ]}"
    ));
}

#[test]
fn out_token_escapes_as_outfuncs_does() {
    let mut s = String::new();
    out_token(&mut s, None);
    out_token(&mut s, Some(""));
    assert_eq!(s, "<><>");
    let tok = |v: &str| {
        let mut s = String::new();
        out_token(&mut s, Some(v));
        s
    };
    assert_eq!(tok("a b(c)"), "a\\ b\\(c\\)");
    assert_eq!(tok("1abc"), "\\1abc");
    assert_eq!(tok("-1"), "\\-1");
    assert_eq!(tok("-x"), "-x");
    assert_eq!(tok("<x"), "\\<x");
    assert_eq!(tok("\"q"), "\\\"q");
}

#[test]
fn no_qualification_and_no_actions() {
    assert_eq!(node_to_string(None), "<>");
    assert_eq!(queries_to_string(&[]), "<>");
}
