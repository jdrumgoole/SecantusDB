//! Ports of mongo's own `key_string_test.cpp` (r8.2.11) cases, plus the
//! ordering property against `secantus-core`'s own BSON comparator.

use super::*;
use bson::{doc, oid::ObjectId, spec::BinarySubtype, Binary, Bson, DateTime, Decimal128, Document};
use std::cmp::Ordering as Ord_;

const ASC: Ordering = Ordering::all_ascending();

fn desc() -> Ordering {
    Ordering::from_directions(&[-1])
}

fn dec(s: &str) -> Bson {
    Bson::Decimal128(
        s.parse::<Decimal128>()
            .unwrap_or_else(|e| panic!("{s}: {e:?}")),
    )
}

fn key(ordering: Ordering, vals: &[Bson]) -> (Vec<u8>, TypeBits) {
    let mut b = Builder::new(Version::V1, ordering);
    for v in vals {
        b.append_bson_element(v).unwrap();
    }
    b.finish().unwrap()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Exact equality: the same BSON types and bytes. NaNs equal themselves; a
/// decimal NaN is any NaN (KeyString keeps one).
fn same(a: &Bson, b: &Bson) -> bool {
    match (a, b) {
        (Bson::Double(x), Bson::Double(y)) => {
            (x.is_nan() && y.is_nan()) || x.to_bits() == y.to_bits()
        }
        (Bson::Decimal128(x), Bson::Decimal128(y)) => {
            x.bytes() == y.bytes()
                || (dec::D::from_bson(x) == dec::D::Nan && dec::D::from_bson(y) == dec::D::Nan)
        }
        (Bson::Array(x), Bson::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
        (Bson::Document(x), Bson::Document(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y.iter())
                    .all(|((k1, v1), (k2, v2))| k1 == k2 && same(v1, v2))
        }
        (Bson::JavaScriptCodeWithScope(x), Bson::JavaScriptCodeWithScope(y)) => {
            x.code == y.code
                && same(
                    &Bson::Document(x.scope.clone()),
                    &Bson::Document(y.scope.clone()),
                )
        }
        _ => a == b,
    }
}

/// `ROUNDTRIP`: encode ascending and descending, decode with the TypeBits as
/// stored (through their serialised form), and get the identical values back;
/// the key size is also checked.
fn roundtrip(vals: &[Bson]) {
    for ordering in [ASC, desc()] {
        let (k, tb) = key(ordering, vals);
        let tb2 = TypeBits::from_buffer(Version::V1, &tb.serialize()).unwrap();
        let got = to_bson(&k, ordering, &tb2).unwrap_or_else(|e| panic!("{vals:?}: {e}"));
        assert_eq!(got.len(), vals.len(), "{vals:?}");
        for (g, v) in got.iter().zip(vals) {
            assert!(same(g, v), "{vals:?}: decoded {g:?}");
        }
        assert_eq!(get_key_size(&k, ordering).unwrap(), k.len(), "{vals:?}");
    }
}

fn rt1(v: Bson) {
    roundtrip(&[v]);
}

// ------------------------------- TypeBits -----------------------------------

#[test]
fn type_bits_sizes_and_forms() {
    let mut tb = TypeBits::new(Version::V1);
    assert!(tb.is_all_zeros());
    assert_eq!(tb.to_buffer(), vec![0]);
    assert!(tb.serialize().is_empty());
    tb.append_string(); // 1 zero bit: still all zeros
    tb.append_number_int();
    assert!(tb.is_all_zeros());
    tb.append_symbol();
    assert!(!tb.is_all_zeros());
    assert_eq!(tb.to_buffer(), vec![0b1000]);
    // 2 bits each for double / long / decimal.
    let mut t = TypeBits::new(Version::V1);
    t.append_number_double();
    assert_eq!(t.to_buffer(), vec![0x01]);
    let mut t = TypeBits::new(Version::V1);
    t.append_number_long();
    assert_eq!(t.to_buffer(), vec![0x02]);
    let mut t = TypeBits::new(Version::V1);
    t.append_number_decimal();
    assert_eq!(t.to_buffer(), vec![0x03]);
    // A decimal zero is 5 + 12 bits (three bytes).
    let mut t = TypeBits::new(Version::V1);
    t.append_decimal_zero(0x1000);
    assert_eq!(t.to_buffer().len(), 1 + 3);
    // The exponent is 6 bits.
    let mut t = TypeBits::new(Version::V1);
    t.append_decimal_exponent(1);
    assert_eq!(t.to_buffer(), vec![0x20]);
    // High bit set in the only data byte: a size byte is needed.
    let mut t = TypeBits::new(Version::V1);
    for _ in 0..4 {
        t.append_number_decimal();
    }
    assert_eq!(t.to_buffer(), vec![0x81, 0xff]);
}

#[test]
fn all_zero_buffers_read_as_all_zeros() {
    for buf in [vec![], vec![0u8], vec![0u8; 16]] {
        let tb = TypeBits::from_buffer(Version::V1, &buf).unwrap();
        assert!(tb.is_all_zeros());
        assert_eq!(tb.to_buffer(), vec![0]);
    }
}

#[test]
fn lots_of_zero_type_bits_stay_short_encoded() {
    let mut tb = TypeBits::new(Version::V1);
    for _ in 0..100_000 {
        tb.append_string();
    }
    assert!(tb.is_all_zeros());
    assert_eq!(tb.to_buffer(), vec![0]);
}

/// `checkKeyWithNByteOfTypeBits`: an array of doubles whose TypeBits are
/// exactly `n` bytes, in every one of the four size encodings.
#[test]
fn keys_with_n_bytes_of_type_bits() {
    for (n, all_zeros) in [
        (0usize, false),
        (1, false),
        (1, true),
        (127, false),
        (127, true),
        (128, false),
        (128, true),
        (129, false),
        (129, true),
    ] {
        // Doubles use two bits ("10"), strings one zero bit: n bytes need 4n
        // doubles, or n*8 strings.
        let arr: Vec<Bson> = if all_zeros {
            vec![Bson::String("x".into()); n * 8]
        } else {
            vec![Bson::Double(1.5); n * 4]
        };
        let v = Bson::Document(doc! {"x": arr});
        let (k, tb) = key(ASC, std::slice::from_ref(&v));
        let size = tb.to_buffer().len();
        if n == 1 || all_zeros {
            assert_eq!(size, 1, "n={n} zeros={all_zeros}");
        } else if n <= 127 {
            assert_eq!(size, n + 1, "n={n}");
        } else {
            assert_eq!(size, n + 5, "n={n}");
        }
        let tb2 = TypeBits::from_buffer(Version::V1, &tb.serialize()).unwrap();
        assert_eq!(tb2.to_buffer(), tb.to_buffer());
        let got = to_bson(&k, ASC, &tb2).unwrap();
        assert!(same(&got[0], &v), "n={n}");
    }
}

#[test]
fn key_with_lots_of_type_bits_round_trips() {
    let arr: Vec<Bson> = vec![Bson::Double(0.0); 54321];
    roundtrip(&[Bson::Document(doc! {"x": arr})]);
}

// ---------------------------- Actual bytes ----------------------------------

#[test]
fn actual_bytes_double() {
    let (k, _) = key(ASC, &[Bson::Double(5.5)]);
    assert_eq!(hex(&k), "2b0b8000000000000004");
    let (k, _) = key(desc(), &[Bson::Double(5.5)]);
    // Every byte is flipped except the end byte.
    assert_eq!(hex(&k), "d4f47fffffffffffff04");
}

#[test]
fn small_values_and_ints() {
    assert_eq!(hex(&key(ASC, &[Bson::Int32(0)]).0), "2904");
    assert_eq!(hex(&key(ASC, &[Bson::Int32(1)]).0), "2b0204");
    assert_eq!(hex(&key(ASC, &[Bson::Int32(-1)]).0), "27fd04");
    assert_eq!(
        hex(&key(ASC, &[Bson::Int64(i64::MIN)]).0),
        "1ff83fffffffffffff04"
    );
}

// ------------------------------ Round trips ----------------------------------

#[test]
fn all_types_simple() {
    rt1(Bson::Double(5.5));
    rt1(Bson::String("abc".into()));
    rt1(Bson::Document(doc! {"a": "b"}));
    rt1(Bson::Array(vec![Bson::Int32(1), Bson::String("x".into())]));
    rt1(Bson::Binary(Binary {
        subtype: BinarySubtype::Generic,
        bytes: vec![1, 2, 3],
    }));
    rt1(Bson::ObjectId(
        ObjectId::parse_str("0123456789abcdef01234567").unwrap(),
    ));
    rt1(Bson::Boolean(true));
    rt1(Bson::Boolean(false));
    rt1(Bson::DateTime(DateTime::from_millis(1_234_567_890_123)));
    rt1(Bson::DateTime(DateTime::from_millis(-1)));
    rt1(Bson::Null);
    rt1(Bson::Undefined);
    rt1(Bson::MinKey);
    rt1(Bson::MaxKey);
    rt1(Bson::Timestamp(bson::Timestamp {
        time: 0,
        increment: 0,
    }));
    rt1(Bson::Timestamp(bson::Timestamp {
        time: u32::MAX,
        increment: u32::MAX,
    }));
    rt1(Bson::RegularExpression(bson::Regex {
        pattern: "a+b".into(),
        options: "im".into(),
    }));
    rt1(Bson::JavaScriptCode("function(){}".into()));
    rt1(Bson::JavaScriptCodeWithScope(
        bson::JavaScriptCodeWithScope {
            code: "x".into(),
            scope: doc! {"y": 1, "z": [1.5, "s"]},
        },
    ));
    rt1(Bson::Symbol("sym".into()));
    rt1(Bson::Int32(7));
    rt1(Bson::Int64(7));
    roundtrip(&[
        Bson::Int32(1),
        Bson::String("two".into()),
        Bson::Double(3.5),
    ]);
}

#[test]
fn db_pointer_round_trips() {
    let p = super::encode::make_db_pointer("db.coll", [7u8; 12]).unwrap();
    rt1(p.clone());
    rt1(Bson::Document(doc! {"p": p}));
}

#[test]
fn array_and_subdoc_cases() {
    rt1(Bson::Array(vec![]));
    rt1(Bson::Array(vec![Bson::Array(vec![
        Bson::Int32(1),
        Bson::Array(vec![]),
    ])]));
    rt1(Bson::Document(doc! {"a": {"b": {"c": [1, {"d": null}]}}}));
    rt1(Bson::Document(doc! {"a": 1, "a": 2}));
    rt1(Bson::Document(doc! {"": 1, "x": Bson::Undefined}));
    rt1(Bson::Document(doc! {"s": "a\0b", "n": f64::NAN}));
}

#[test]
fn embedded_null_string() {
    let s = Bson::String("a\0b\0".into());
    rt1(s.clone());
    let (k, _) = key(ASC, &[s]);
    assert_eq!(hex(&k), "3c6100ff6200ff0004");
    let (k, _) = key(desc(), &[Bson::String("a\0b".into())]);
    // 3c 61 00ff 62 00 : each byte but the end byte inverted.
    assert_eq!(hex(&k), "c39eff009dff04");
    // A string with a NUL sorts after the string it extends only by byte.
    let a = key(ASC, &[Bson::String("a".into())]).0;
    let anul = key(ASC, &[Bson::String("a\0".into())]).0;
    assert!(a < anul);
}

#[test]
fn very_large_string() {
    rt1(Bson::String("x".repeat(123_456)));
    rt1(Bson::Binary(Binary {
        subtype: BinarySubtype::Generic,
        bytes: vec![9; 300],
    }));
    rt1(Bson::Binary(Binary {
        subtype: BinarySubtype::Generic,
        bytes: vec![9; 254],
    }));
    rt1(Bson::Binary(Binary {
        subtype: BinarySubtype::Generic,
        bytes: vec![9; 255],
    }));
}

#[test]
fn numbers_near_int32_max() {
    let start = i64::from(i32::MAX);
    for i in -1000i64..1000 {
        let v = start + i;
        rt1(Bson::Int64(v));
        rt1(Bson::Double(v as f64));
        if let Ok(v32) = i32::try_from(v) {
            rt1(Bson::Int32(v32));
        }
    }
}

#[test]
fn lots_of_numbers_1() {
    for i in 0..64u32 {
        let x = 1i64.wrapping_shl(i);
        let xd = (1u128 << i) as f64;
        if i < 63 {
            rt1(Bson::Int64(x));
            rt1(Bson::Int64(x + 1));
            rt1(Bson::Int64(-x));
            rt1(Bson::Int64(-(x + 1)));
        } else {
            rt1(Bson::Int64(i64::MIN));
        }
        if i < 31 {
            rt1(Bson::Int32(x as i32));
            rt1(Bson::Int32(-(x as i32)));
            rt1(Bson::Int32(x as i32 + 1));
        }
        for d in [xd, xd + 0.1, xd - 0.1, xd + 1.0, xd + 1.1] {
            rt1(Bson::Double(d));
            rt1(Bson::Double(-d));
        }
    }
}

#[test]
fn lots_of_numbers_2() {
    for i in -1100..1100 {
        rt1(Bson::Double(2f64.powi(i)));
        rt1(Bson::Double(2.1f64.powi(i)));
    }
}

#[test]
fn nans_collapse_to_one_key() {
    let nan1 = f64::NAN;
    let nan2 = f64::from_bits(0x7ff0_0000_0000_0001); // signalling
    let nan_neg = f64::from_bits(0xfff8_0000_0000_0000);
    for o in [ASC, desc()] {
        let a = key(o, &[Bson::Double(nan1)]);
        let b = key(o, &[Bson::Double(nan2)]);
        let c = key(o, &[Bson::Double(nan_neg)]);
        assert_eq!(a.0, b.0);
        assert_eq!(a.0, c.0);
        let tb = TypeBits::from_buffer(Version::V1, &a.1.serialize()).unwrap();
        let got = to_bson(&a.0, o, &tb).unwrap();
        assert!(matches!(got[0], Bson::Double(d) if d.is_nan()));
    }
    // Decimal NaNs (positive and negative) share a key with the double NaN.
    let dn = Decimal128::from_bytes({
        let mut b = [0u8; 16];
        b[15] = 0xfc; // negative quiet NaN
        b
    });
    assert_eq!(
        key(ASC, &[Bson::Decimal128(dn)]).0,
        key(ASC, &[Bson::Double(nan1)]).0
    );
    rt1(dec("NaN"));
}

// ------------------------------- Decimals -------------------------------------

#[test]
fn decimal_numbers() {
    for s in [
        // Zeros
        "0",
        "0.0",
        "-0",
        "0E5000",
        "-0.0000E-6172",
        // Special numbers
        "NaN",
        "+Inf",
        "-Inf",
        // Whole doubles
        "1",
        "2.0",
        "-2.0E1",
        "1234.56E15",
        "2.00000000000000000000000",
        "-9223372036854775808.00000000000000",
        "973555660975280180349468061728768E1",
        // Fractional doubles
        "1.25",
        "3.141592653584666550159454345703125",
        "-127.50",
        // Whole int64 non-doubles
        "243290200817664E4",
        "9007199254740993",
        "9223372036854775807",
        "-9223372036854775808",
        // In the int64 range without a double or integer representation
        "1.23",
        "-1.1",
        "-12345.60",
        "3.141592653589793238462643383279502",
        "-3.141592653589793115997963468544185",
        // Small doubles
        "0.50",
        "-0.3552713678800500929355621337890625E-14",
        "-0.000000000000001234567890123456789012345678901234E-99",
        // Small decimals not representable as double
        "0.02",
        // Large decimals
        "1234567890123456789012345678901234E6000",
        "-19950631168.80758384883742162683585E3000",
        // Tiny decimals
        "0.2512388057698744585180135042133610E-6020",
        "4.940656458412465441765687928682213E-324",
        "-0.8289046058458094980903836776809409E-316",
    ] {
        rt1(dec(s));
    }
    roundtrip(&[
        dec("4.940656458412465441765687928682213E-324"),
        Bson::Int32(1),
    ]);
    roundtrip(&[Bson::Null, Bson::Document(doc! {"a": dec("Infinity")})]);
}

#[test]
fn decimals_round_trip_across_the_exponent_range() {
    // Decimals at assorted exponents and digit counts (the exponent lives in
    // the TypeBits, so every one must come back with its own).
    let mut seed = 0x9e3779b97f4a7c15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..4000 {
        let digits = 1 + (next() % 34) as usize;
        let mut coef = String::new();
        for i in 0..digits {
            let d = (next() % 10) as u8;
            coef.push((b'0' + if i == 0 && d == 0 { 1 } else { d }) as char);
        }
        let exp = match next() % 4 {
            0 => (next() % 80) as i32 - 40,
            1 => (next() % 600) as i32 - 300,
            2 => (next() % 12000) as i32 - 6100,
            _ => (next() % 40) as i32 - 20 - digits as i32,
        };
        let neg = if next() % 2 == 0 { "-" } else { "" };
        let s = format!("{neg}{coef}E{exp}");
        let Ok(d) = s.parse::<Decimal128>() else {
            continue;
        };
        let b = Bson::Decimal128(d);
        // Only values whose exponent is within range survive the parse.
        if dec::D::from_bson(&d).to_big().is_some() {
            rt1(b);
        }
    }
}

#[test]
fn decimals_equal_to_a_double_share_its_key() {
    for (d, f) in [
        ("1", 1.0),
        ("1.5", 1.5),
        ("0.5", 0.5),
        ("-2.5", -2.5),
        ("100", 100.0),
        ("1E+22", 1e22),
        ("9007199254740992", 9007199254740992.0),
    ] {
        for o in [ASC, desc()] {
            assert_eq!(key(o, &[dec(d)]).0, key(o, &[Bson::Double(f)]).0, "{d}");
        }
    }
    // 0.1 is not the double 0.1.
    assert_ne!(key(ASC, &[dec("0.1")]).0, key(ASC, &[Bson::Double(0.1)]).0);
    assert!(key(ASC, &[dec("0.1")]).0 < key(ASC, &[Bson::Double(0.1)]).0);
    // A decimal and the int of the same value are one key; precision is TypeBits.
    assert_eq!(key(ASC, &[dec("1.00")]).0, key(ASC, &[Bson::Int32(1)]).0);
    assert_ne!(key(ASC, &[dec("1.00")]).1, key(ASC, &[dec("1.0")]).1);
}

#[test]
fn decimal_zero_type_bits_carry_the_exponent_and_sign() {
    for s in [
        "0", "0.0", "0.00", "-0", "-0.0", "0E-6176", "0E+6111", "-0E+6111",
    ] {
        let b = dec(s);
        let (k, tb) = key(ASC, std::slice::from_ref(&b));
        assert_eq!(hex(&k), "2904", "{s}");
        let tb = TypeBits::from_buffer(Version::V1, &tb.serialize()).unwrap();
        let got = to_bson(&k, ASC, &tb).unwrap();
        assert!(same(&got[0], &b), "{s}");
    }
    // Distinct zeros have distinct TypeBits.
    assert_ne!(key(ASC, &[dec("0")]).1, key(ASC, &[dec("0.0")]).1);
    assert_ne!(key(ASC, &[dec("0")]).1, key(ASC, &[dec("-0")]).1);
}

// -------------------------------- Ordering ------------------------------------

#[test]
fn numeric_types_compare_by_value() {
    let ascending = [
        Bson::Double(f64::NEG_INFINITY),
        dec("-1E+6000"),
        Bson::Double(-1e300),
        Bson::Int64(i64::MIN),
        Bson::Int64(-(1 << 53) - 1),
        Bson::Double(-1.5),
        Bson::Int32(-1),
        dec("-0.1"),
        Bson::Double(-1e-300),
        dec("-1E-6000"),
        Bson::Int32(0),
        dec("1E-6000"),
        Bson::Double(5e-324),
        Bson::Double(1e-300),
        dec("0.1"),
        Bson::Double(0.1),
        Bson::Double(0.5),
        Bson::Int32(1),
        Bson::Double(1.5),
        Bson::Int32(2),
        Bson::Int64((1 << 53) + 1),
        Bson::Double(9.223_372_036_854_776e18),
        dec("9223372036854775808.5"),
        Bson::Double(1e300),
        dec("1E+6000"),
        Bson::Double(f64::INFINITY),
    ];
    for o in [ASC, desc()] {
        let keys: Vec<_> = ascending
            .iter()
            .map(|v| key(o, std::slice::from_ref(v)).0)
            .collect();
        for w in keys.windows(2) {
            if o == ASC {
                assert!(w[0] < w[1], "{} !< {}", hex(&w[0]), hex(&w[1]));
            } else {
                assert!(w[0] > w[1], "{} !> {}", hex(&w[0]), hex(&w[1]));
            }
        }
    }
    // NaN sorts below every number, -Infinity included.
    assert!(key(ASC, &[Bson::Double(f64::NAN)]).0 < key(ASC, &[Bson::Double(f64::NEG_INFINITY)]).0);
}

#[test]
fn types_sort_in_canonical_order() {
    let ascending = [
        Bson::MinKey,
        Bson::Undefined,
        Bson::Null,
        Bson::Int32(5),
        Bson::String("a".into()),
        Bson::Document(doc! {}),
        Bson::Array(vec![]),
        Bson::Binary(Binary {
            subtype: BinarySubtype::Generic,
            bytes: vec![],
        }),
        Bson::ObjectId(ObjectId::from_bytes([0; 12])),
        Bson::Boolean(false),
        Bson::Boolean(true),
        Bson::DateTime(DateTime::from_millis(0)),
        Bson::Timestamp(bson::Timestamp {
            time: 0,
            increment: 1,
        }),
        Bson::RegularExpression(bson::Regex {
            pattern: "a".into(),
            options: "".into(),
        }),
        super::encode::make_db_pointer("a", [0; 12]).unwrap(),
        Bson::JavaScriptCode("a".into()),
        Bson::JavaScriptCodeWithScope(bson::JavaScriptCodeWithScope {
            code: "a".into(),
            scope: doc! {},
        }),
        Bson::MaxKey,
    ];
    let keys: Vec<_> = ascending
        .iter()
        .map(|v| key(ASC, std::slice::from_ref(v)).0)
        .collect();
    for w in keys.windows(2) {
        assert!(w[0] < w[1], "{} !< {}", hex(&w[0]), hex(&w[1]));
    }
}

#[test]
fn record_id_order_follows_the_key() {
    let k = |ord, vals: &[Bson], rid| {
        let mut b = Builder::new(Version::V1, ord);
        for v in vals {
            b.append_bson_element(v).unwrap();
        }
        b.append_record_id_long(rid).unwrap();
        b.finish().unwrap().0
    };
    let a = k(ASC, &[Bson::Int32(5)], 2);
    let b = k(ASC, &[Bson::Int32(5)], 3);
    let c = k(ASC, &[Bson::Int32(6)], 1);
    assert!(a < b && b < c);
    let o2 = Ordering::from_directions(&[-1, -1]);
    let a = k(o2, &[Bson::Int32(5), Bson::Int32(6)], 5);
    let b = k(o2, &[Bson::Int32(5), Bson::Int32(5)], 4);
    let c = k(o2, &[Bson::Int32(3), Bson::Int32(4)], 3);
    assert!(a < b && b < c);
}

#[test]
fn discriminators() {
    let build = |d| {
        let mut b = Builder::with_discriminator(Version::V1, ASC, d);
        b.append_bson_element(&Bson::Int32(1)).unwrap();
        b.finish().unwrap().0
    };
    assert_eq!(hex(&build(Discriminator::Inclusive)), "2b0204");
    assert_eq!(hex(&build(Discriminator::ExclusiveBefore)), "2b0201");
    assert_eq!(hex(&build(Discriminator::ExclusiveAfter)), "2b02fe");
    // Discriminators are never inverted, even for a descending key.
    let mut b = Builder::with_discriminator(Version::V1, desc(), Discriminator::ExclusiveAfter);
    b.append_bson_element(&Bson::Int32(1)).unwrap();
    assert_eq!(hex(&b.finish().unwrap().0), "d4fd".to_string() + "fe");
    // The sorting contract.
    let before = build(Discriminator::ExclusiveBefore);
    let incl = build(Discriminator::Inclusive);
    let after = build(Discriminator::ExclusiveAfter);
    let mut with_rid = incl.clone();
    with_rid.extend_from_slice(&[0x00, 0x08]);
    assert!(before < incl && incl < with_rid && with_rid < after);
    assert_eq!(
        without_discriminator_at_end(&after).unwrap(),
        &after[..after.len() - 1]
    );
}

// -------------------------------- RecordIds -----------------------------------

#[test]
fn record_ids_every_width() {
    for i in 0..63 {
        let rid = 1i64 << i;
        let enc = |r: i64| {
            let mut b = Builder::new(Version::V1, ASC);
            b.append_record_id_long(r).unwrap();
            b.finish().unwrap().0
        };
        let ks = enc(rid);
        assert!((2..=10).contains(&ks.len()));
        assert_eq!(decode_record_id_long(&ks).unwrap(), (rid, ks.len()));
        assert_eq!(without_record_id_long_at_end(&ks).unwrap(), (&[][..], rid));
        assert!(enc(1) <= ks);
        assert!(enc(rid - 1) < ks);
        assert!(ks < enc(i64::MAX) || rid == i64::MAX);
        for j in 0..63 {
            let other = 1i64 << j;
            assert_eq!(rid.cmp(&other), enc(rid).cmp(&enc(other)));
            // Concatenated RecordIds, decoded back to front and front to back.
            let mut cat = Vec::new();
            for r in [i64::MAX, rid, 0xDEADBEEF, rid, 1, rid, other] {
                cat.extend_from_slice(&enc(r));
            }
            assert_eq!(without_record_id_long_at_end(&cat).unwrap().1, other);
            let mut at = 0;
            for want in [i64::MAX, rid, 0xDEADBEEF, rid, 1, rid, other] {
                let (got, n) = decode_record_id_long(&cat[at..]).unwrap();
                assert_eq!(got, want);
                at += n;
            }
            assert_eq!(at, cat.len());
        }
    }
}

#[test]
fn string_record_ids() {
    for size in [1usize, 12, 127, 128, 16383, 16384, 100_000] {
        let rid: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        let mut b = Builder::new(Version::V1, ASC);
        b.append_bson_element(&Bson::String("k".into())).unwrap();
        b.append_record_id_bytes(&rid).unwrap();
        let (key, _) = b.finish().unwrap();
        let (rest, got) = without_record_id_str_at_end(&key).unwrap();
        assert_eq!(got, &rid[..]);
        assert_eq!(
            rest,
            &key[..key.len()
                - size
                - if size < 128 {
                    1
                } else if size < 16384 {
                    2
                } else {
                    3
                }]
        );
        assert_eq!(decode_record_id_str_at_end(&key).unwrap(), &rid[..]);
        // The size bytes: 7-bit groups, most significant last, 0x80 on all
        // but the last.
        if size == 128 {
            assert_eq!(&key[key.len() - 2..], &[0x00, 0x81]);
        }
    }
    // The 12-byte case of mongo's RecordIdStr test: 12 + 1 size byte + 1 end.
    let mut b = Builder::new(Version::V1, ASC);
    b.append_record_id_bytes(&[0x80; 12]).unwrap();
    assert_eq!(b.finish().unwrap().0.len(), 13);
}

#[test]
fn string_record_id_compares_bytewise() {
    let k = |r: &[u8]| {
        let mut b = Builder::new(Version::V1, ASC);
        b.append_record_id_bytes(r).unwrap();
        b.finish().unwrap().0
    };
    let mut lo = [0x80u8; 12];
    lo[11] = 0x7f;
    let hi = [0x80u8; 12];
    assert!(k(&lo) < k(&hi));
    assert!(k(&[0; 12]) < k(&[0xff; 12]));
}

// ---------------------------- Malformed input ---------------------------------

#[test]
fn malformed_buffers_are_errors_not_panics() {
    let tb = TypeBits::new(Version::V1);
    for buf in [
        &[][..],
        &[0xff][..],
        &[ctype::STRING_LIKE, b'a'][..],
        &[ctype::NUMERIC_POSITIVE_8_BYTE_INT, 1, 2][..],
        &[ctype::NUMERIC_POSITIVE_SMALL_MAGNITUDE, 1][..],
        &[ctype::OBJECT, b'x'][..],
    ] {
        // An empty buffer decodes to no values; anything else must not panic.
        let r = to_bson(buf, ASC, &tb);
        if buf.is_empty() {
            assert_eq!(r.unwrap(), vec![]);
        } else {
            assert!(r.is_err(), "{buf:?}");
        }
    }
    assert!(TypeBits::from_buffer(Version::V1, &[0x85]).is_err());
    assert!(TypeBits::from_buffer(Version::V1, &[0x80, 1, 0, 0, 0]).is_err()); // overlong
                                                                               // A decimal exponent that cannot be applied.
    let mut b = Builder::new(Version::V1, ASC);
    b.append_bson_element(&dec("1.5")).unwrap();
    let (k, _) = b.finish().unwrap();
    assert!(to_bson(&k, ASC, &TypeBits::new(Version::V1)).is_err()); // Decimal bit missing
    assert!(without_record_id_long_at_end(&[1]).is_err());
    assert!(without_record_id_str_at_end(&[]).is_err());
}

#[test]
fn symbol_with_collation_is_refused() {
    let t = |s: &str| s.as_bytes().iter().rev().copied().collect::<Vec<u8>>();
    let mut b = Builder::new(Version::V1, ASC);
    let err = b
        .append_bson_element_with(&Bson::Symbol("a".into()), Some(&t))
        .unwrap_err();
    assert!(matches!(err, Error::CannotBuildIndexKeys(_)));
    // Strings (nested ones too) go through the collation's key function.
    let mut b = Builder::new(Version::V1, ASC);
    b.append_bson_element_with(&Bson::Document(doc! {"a": "xyz"}), Some(&t))
        .unwrap();
    let (k, _) = b.finish().unwrap();
    assert_eq!(
        hex(&k),
        "46".to_string() + "3c" + "6100" + "3c" + "7a797800" + "00" + "04"
    );
}

// ---------------------------- The ordering property ---------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn gen_decimal(r: &mut Rng) -> Bson {
    match r.below(10) {
        0 => dec("NaN"),
        1 => dec(["Infinity", "-Infinity"][r.below(2) as usize]),
        2 => dec(["0", "-0", "0.00", "0E+100", "-0E-100"][r.below(5) as usize]),
        3 => {
            // Equal to a double.
            let f = f64::from_bits(r.next());
            if f.is_finite() {
                dec(&format!("{f:e}"))
            } else {
                dec("1")
            }
        }
        4 => dec(&format!("{}.{}", r.below(1000) as i64 - 500, r.below(1000))),
        _ => {
            let digits = 1 + r.below(34) as usize;
            let mut c = String::new();
            for i in 0..digits {
                let d = r.below(10) as u8;
                c.push((b'0' + if i == 0 && d == 0 { 1 } else { d }) as char);
            }
            let exp = match r.below(3) {
                0 => r.below(60) as i64 - 30 - digits as i64,
                1 => r.below(700) as i64 - 350,
                _ => r.below(12000) as i64 - 6100,
            };
            let neg = if r.below(2) == 0 { "-" } else { "" };
            match format!("{neg}{c}E{exp}").parse::<Decimal128>() {
                Ok(d) => Bson::Decimal128(d),
                Err(_) => dec("1"),
            }
        }
    }
}

fn gen_double(r: &mut Rng) -> f64 {
    match r.below(12) {
        0 => f64::from_bits(r.next()),
        1 => (r.below(2_000_000) as f64 - 1_000_000.0) / 2.0,
        2 => 2f64.powi(r.below(2200) as i32 - 1100),
        3 => f64::from_bits(r.below(1 << 52)), // subnormal
        4 => [f64::INFINITY, f64::NEG_INFINITY, f64::NAN, -0.0, 0.0][r.below(5) as usize],
        5 => (r.next() >> 11) as f64,
        6 => ((1u64 << 53) + r.below(100)) as f64,
        7 => 2f64.powi(63) + (r.below(1 << 12) as f64) * 1024.0,
        8 => r.below(1000) as f64 / 1000.0,
        9 => (r.below(1000) as f64 / 7.0) * 10f64.powi(r.below(60) as i32 - 30),
        10 => f64::MAX / (r.below(5) + 1) as f64,
        _ => (r.below(200) as f64 - 100.0) * 1.5,
    }
}

fn gen_string(r: &mut Rng) -> String {
    let n = r.below(6) as usize;
    (0..n)
        .map(|_| match r.below(6) {
            0 => '\0',
            1 => 'é',
            2 => '漢',
            3 => '\u{10348}',
            _ => (b'a' + r.below(4) as u8) as char,
        })
        .collect()
}

fn gen_scalar(r: &mut Rng) -> Bson {
    match r.below(18) {
        0 => Bson::MinKey,
        1 => Bson::MaxKey,
        2 => Bson::Null,
        3 => Bson::Int32([0, 1, -1, i32::MAX, i32::MIN, r.next() as i32][r.below(6) as usize]),
        4 => Bson::Int64(
            [
                0i64,
                i64::MAX,
                i64::MIN,
                (1 << 53) + 1,
                -(1 << 53) - 1,
                r.next() as i64,
                (r.next() >> r.below(60)) as i64,
            ][r.below(7) as usize],
        ),
        5 | 6 => Bson::Double(gen_double(r)),
        7 | 8 => gen_decimal(r),
        9 | 10 => Bson::String(gen_string(r)),
        11 => {
            let n = r.below(4) as usize;
            Bson::Binary(Binary {
                subtype: if r.below(2) == 0 {
                    BinarySubtype::Generic
                } else {
                    BinarySubtype::Uuid
                },
                bytes: (0..n).map(|_| r.below(3) as u8 * 100).collect(),
            })
        }
        12 => Bson::ObjectId(ObjectId::from_bytes([r.below(3) as u8; 12])),
        13 => Bson::Boolean(r.below(2) == 0),
        14 => Bson::DateTime(DateTime::from_millis(r.next() as i64 >> r.below(40))),
        15 => Bson::Timestamp(bson::Timestamp {
            time: r.below(3) as u32,
            increment: r.below(3) as u32,
        }),
        16 => Bson::RegularExpression(bson::Regex {
            // A regex pattern cannot contain a NUL.
            pattern: gen_string(r).replace('\0', "z"),
            options: ["", "i", "im"][r.below(3) as usize].into(),
        }),
        _ => Bson::JavaScriptCode(gen_string(r)),
    }
}

fn gen_value(r: &mut Rng, depth: u32) -> Bson {
    if depth < 2 && r.below(6) == 0 {
        if r.below(2) == 0 {
            let n = r.below(3) as usize;
            Bson::Array((0..n).map(|_| gen_value(r, depth + 1)).collect())
        } else {
            let n = r.below(3) as usize;
            let mut d = Document::new();
            for _ in 0..n {
                d.insert(
                    ["a", "b", "", "ab"][r.below(4) as usize],
                    gen_value(r, depth + 1),
                );
            }
            Bson::Document(d)
        }
    } else if depth < 2 && r.below(40) == 0 {
        Bson::JavaScriptCodeWithScope(bson::JavaScriptCodeWithScope {
            code: gen_string(r),
            scope: doc! {"s": gen_scalar(r)},
        })
    } else {
        gen_scalar(r)
    }
}

/// For a few thousand generated values: byte order of the keys equals the BSON
/// comparison order `secantus-core` implements, ascending and descending, and
/// values the comparator calls equal share a key.
#[test]
fn key_order_equals_bson_order() {
    let mut r = Rng(0x2545_f491_4f6c_dd1d);
    let mut pool: Vec<Bson> = (0..900).map(|_| gen_value(&mut r, 0)).collect();
    // Hand-picked boundaries the generator may miss.
    pool.extend([
        Bson::Double(0.1),
        dec("0.1"),
        dec("0.10000000000000000555111512312578"),
        Bson::Double(9007199254740992.0),
        Bson::Int64(9007199254740993),
        dec("9007199254740993"),
        dec("9007199254740992.5"),
        dec("9223372036854775807"),
        dec("9223372036854775808"),
        Bson::Double(9223372036854775808.0),
        Bson::Int64(i64::MAX),
        dec("1E-400"),
        dec("1E-330"),
        Bson::Double(5e-324),
        dec("4.9E-324"),
        dec("4.940656458412465441765687928682213E-324"),
        dec("1.797693134862315708145274237317043E308"),
        dec("1.797693134862315708145274237317044E308"),
        Bson::Double(f64::MAX),
        dec("1E+400"),
    ]);
    let keys_asc: Vec<Vec<u8>> = pool
        .iter()
        .map(|v| key(ASC, std::slice::from_ref(v)).0)
        .collect();
    let keys_desc: Vec<Vec<u8>> = pool
        .iter()
        .map(|v| key(desc(), std::slice::from_ref(v)).0)
        .collect();
    let mut checked = 0u64;
    for i in 0..pool.len() {
        for j in 0..pool.len() {
            let want = crate::order::cmp(&pool[i], &pool[j]);
            let got_a = keys_asc[i].cmp(&keys_asc[j]);
            let got_d = keys_desc[j].cmp(&keys_desc[i]);
            assert!(
                want == got_a && want == got_d,
                "{:?} vs {:?}: BSON order {:?}, ascending keys {:?} ({} vs {}), descending {:?}",
                pool[i],
                pool[j],
                want,
                got_a,
                hex(&keys_asc[i]),
                hex(&keys_asc[j]),
                got_d
            );
            checked += 1;
        }
    }
    assert!(checked > 800_000);
}

/// Compound keys: each field sorts in its own direction, first field first.
#[test]
fn compound_key_order_follows_each_directions() {
    let mut r = Rng(0xdead_beef_cafe_f00d);
    let pool: Vec<(Bson, Bson)> = (0..250)
        .map(|_| (gen_scalar(&mut r), gen_scalar(&mut r)))
        .collect();
    for dirs in [[1, 1], [1, -1], [-1, 1], [-1, -1]] {
        let o = Ordering::from_directions(&dirs);
        let keys: Vec<Vec<u8>> = pool
            .iter()
            .map(|(a, b)| key(o, &[a.clone(), b.clone()]).0)
            .collect();
        for i in 0..pool.len() {
            for j in 0..pool.len() {
                let c1 = crate::order::cmp(&pool[i].0, &pool[j].0);
                let c2 = crate::order::cmp(&pool[i].1, &pool[j].1);
                let apply = |c: Ord_, d: i32| if d < 0 { c.reverse() } else { c };
                let want = apply(c1, dirs[0]).then(apply(c2, dirs[1]));
                assert_eq!(
                    want,
                    keys[i].cmp(&keys[j]),
                    "{:?} vs {:?} under {dirs:?}",
                    pool[i],
                    pool[j]
                );
            }
        }
    }
}

/// Every generated value survives encode -> decode exactly, in both
/// directions, as a stored index entry (key + RecordId + serialised TypeBits).
#[test]
fn generated_values_round_trip_through_a_stored_entry() {
    let mut r = Rng(0x0123_4567_89ab_cdef);
    for n in 0..3000 {
        let v = gen_value(&mut r, 0);
        for o in [ASC, desc()] {
            let mut b = Builder::new(Version::V1, o);
            b.append_bson_element(&v).unwrap();
            b.append_record_id_long(n + 1).unwrap();
            let (key, tb) = b.finish().unwrap();
            let (rest, rid) = without_record_id_long_at_end(&key).unwrap();
            assert_eq!(rid, n + 1);
            let tb = TypeBits::from_buffer(Version::V1, &tb.serialize()).unwrap();
            let got = to_bson(rest, o, &tb).unwrap_or_else(|e| panic!("{v:?}: {e}"));
            assert!(same(&got[0], &v), "{v:?} came back as {:?}", got[0]);
            assert_eq!(get_key_size(&key, o).unwrap(), rest.len());
        }
    }
}
