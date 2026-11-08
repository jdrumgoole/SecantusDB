//! The geometric `box` type and PostgreSQL's `float8` text rendering.
//!
//! A box is four `float8`s -- the upper-right corner then the lower-left --
//! carried as a tagged document so a value knows it is a box wherever it
//! travels: inside an array (whose text form joins boxes with `;`, not `,`),
//! through a cast, out to the wire. Every rule here was measured on
//! PostgreSQL 16 (`select '(1,2),(3,4)'::box` is `(3,4),(1,2)`).

use bson::Bson;

use crate::{Error, Result};

/// The single key of a box document; the value is the four corner coordinates
/// as doubles, `[high.x, high.y, low.x, low.y]`.
pub const BOX_KEY: &str = "__box";

/// A box value from its (already normalised) corners.
pub fn box_value(coords: [f64; 4]) -> Bson {
    let mut d = bson::Document::new();
    d.insert(
        BOX_KEY,
        Bson::Array(coords.iter().map(|c| Bson::Double(*c)).collect()),
    );
    Bson::Document(d)
}

/// The corners of a box value, or `None` for any other value.
pub fn box_coords(v: &Bson) -> Option<[f64; 4]> {
    match v {
        Bson::Document(d) if d.len() == 1 => match d.get(BOX_KEY) {
            Some(Bson::Array(items)) if items.len() == 4 => {
                let mut out = [0.0; 4];
                for (slot, item) in out.iter_mut().zip(items) {
                    *slot = item.as_f64()?;
                }
                Some(out)
            }
            _ => None,
        },
        _ => None,
    }
}

/// Whether a value is a box.
pub fn is_box(v: &Bson) -> bool {
    box_coords(v).is_some()
}

/// `box_in`: `(x1,y1),(x2,y2)`, with the outer parentheses and the inner ones
/// each optional (`((1,2),(3,4))` and `1,2,3,4` are the same box), whitespace
/// allowed around every token. The stored box puts the upper-right corner
/// first: per coordinate the greater value is "high", and a NaN counts as
/// greater than everything (`'(2,3),(nan,1)'` is `(NaN,3),(2,1)`), the order
/// PostgreSQL's float comparison gives it.
pub fn parse_box(text: &str) -> Result<[f64; 4]> {
    let bad = || Error::InvalidText(format!("invalid input syntax for type box: \"{text}\""));
    let chars: Vec<char> = text.chars().collect();
    // A leading `(` is the outer pair's only when a second `(` follows it.
    let mut probe = Scanner {
        chars: chars.clone(),
        pos: 0,
    };
    probe.skip_space();
    let outer = probe.eat('(') && {
        probe.skip_space();
        probe.peek() == Some('(')
    };
    let mut p = Scanner { chars, pos: 0 };
    p.skip_space();
    if outer {
        p.eat('(');
    }
    let (x1, y1) = p.point(bad)?;
    p.skip_space();
    if !p.eat(',') {
        return Err(bad());
    }
    let (x2, y2) = p.point(bad)?;
    p.skip_space();
    if outer && !p.eat(')') {
        return Err(bad());
    }
    p.skip_space();
    if p.pos != p.chars.len() {
        return Err(bad());
    }
    let order = |a: f64, b: f64| {
        if !a.is_nan() && (b.is_nan() || a < b) {
            (b, a)
        } else {
            (a, b)
        }
    };
    let (hx, lx) = order(x1, x2);
    let (hy, ly) = order(y1, y2);
    Ok([hx, hy, lx, ly])
}

/// `box_out`: `(high.x,high.y),(low.x,low.y)`, each number in `float8` text.
pub fn box_text(coords: &[f64; 4]) -> String {
    format!(
        "({},{}),({},{})",
        float8_text(coords[0]),
        float8_text(coords[1]),
        float8_text(coords[2]),
        float8_text(coords[3])
    )
}

struct Scanner {
    chars: Vec<char>,
    pos: usize,
}

impl Scanner {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn skip_space(&mut self) {
        while self.peek().is_some_and(|c| c.is_ascii_whitespace()) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// `(x,y)` or `x,y`, whitespace allowed anywhere between tokens.
    fn point(&mut self, bad: impl Fn() -> Error + Copy) -> Result<(f64, f64)> {
        self.skip_space();
        let parens = self.eat('(');
        let x = self.number(bad)?;
        self.skip_space();
        if !self.eat(',') {
            return Err(bad());
        }
        let y = self.number(bad)?;
        self.skip_space();
        if parens && !self.eat(')') {
            return Err(bad());
        }
        Ok((x, y))
    }

    /// One `float8` token, which runs to the next delimiter.
    fn number(&mut self, bad: impl Fn() -> Error) -> Result<f64> {
        self.skip_space();
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|c| !matches!(c, ',' | '(' | ')') && !c.is_ascii_whitespace())
        {
            self.pos += 1;
        }
        let token: String = self.chars[start..self.pos].iter().collect();
        match parse_float8(&token) {
            Float8Token::Value(v) => Ok(v),
            Float8Token::OutOfRange => Err(Error::NumericOutOfRange(format!(
                "\"{token}\" is out of range for type double precision"
            ))),
            Float8Token::Invalid => Err(bad()),
        }
    }
}

/// What `float8in` makes of one token.
pub enum Float8Token {
    Value(f64),
    /// A decimal too large or too small for a double (`1e400`, `1e-400`):
    /// PostgreSQL's 22003, not a syntax error.
    OutOfRange,
    Invalid,
}

/// `float8in` for one token: a decimal or the special spellings (`NaN`,
/// `Infinity`, `inf`, signed), any case.
pub fn parse_float8(token: &str) -> Float8Token {
    let lower = token.to_ascii_lowercase();
    let unsigned = lower.trim_start_matches(['+', '-']);
    let special = matches!(unsigned, "nan" | "inf" | "infinity");
    let Ok(v) = lower.parse::<f64>() else {
        return Float8Token::Invalid;
    };
    if v.is_infinite() && !special {
        return Float8Token::OutOfRange;
    }
    // A mantissa with a nonzero digit that still parsed to zero underflowed.
    let mantissa = unsigned.split(['e', 'E']).next().unwrap_or("");
    if v == 0.0 && mantissa.chars().any(|c| c.is_ascii_digit() && c != '0') {
        return Float8Token::OutOfRange;
    }
    Float8Token::Value(v)
}

/// `float8out` at the default `extra_float_digits = 1`: the shortest text
/// that round-trips, fixed notation for decimal exponents in `-4..15` and
/// `d.ddde+XX` outside it (`1e+20`, `1e-07`, `100000000000000` for 1e14,
/// `1e+15`). A whole number carries no `.0`; the specials are `NaN`,
/// `Infinity` and `-Infinity`; negative zero keeps its sign.
pub fn float8_text(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_string();
    }
    if v.is_infinite() {
        return (if v > 0.0 { "Infinity" } else { "-Infinity" }).to_string();
    }
    if v == 0.0 {
        return (if v.is_sign_negative() { "-0" } else { "0" }).to_string();
    }
    // Rust's `{:e}` is the shortest round-trip form: `1.2345678901234568e17`.
    shortest_float_text(v < 0.0, format!("{:e}", v.abs()), 15)
}

/// `float4out`: the same shortest round-trip rendering over the SINGLE
/// precision value, which has fewer digits (`0.33333334`, `3.1415927`) and
/// switches to `d.ddde+XX` sooner -- fixed notation only for decimal
/// exponents in `-4..6`, so `1.234567e+06` where float8 prints `1234567`
/// (measured on 16: `16777217::float4` is `1.6777216e+07`).
pub fn float4_text(v: f32) -> String {
    if v.is_nan() {
        return "NaN".to_string();
    }
    if v.is_infinite() {
        return (if v > 0.0 { "Infinity" } else { "-Infinity" }).to_string();
    }
    if v == 0.0 {
        return (if v.is_sign_negative() { "-0" } else { "0" }).to_string();
    }
    shortest_float_text(v < 0.0, format!("{:e}", v.abs()), 6)
}

/// Lay out a `d.ddde<exp>` shortest form the way `float8out` / `float4out`
/// do: fixed notation while the decimal exponent is in `-4..fixed_limit`,
/// `d.ddde+XX` outside it.
fn shortest_float_text(negative: bool, sci: String, fixed_limit: i32) -> String {
    let (mantissa, exp) = sci.split_once('e').expect("exponent form");
    let exp: i32 = exp.parse().expect("integer exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let sign = if negative { "-" } else { "" };
    if !(-4..fixed_limit).contains(&exp) {
        let (first, rest) = digits.split_at(1);
        let frac = if rest.is_empty() {
            String::new()
        } else {
            format!(".{rest}")
        };
        let esign = if exp < 0 { '-' } else { '+' };
        return format!("{sign}{first}{frac}e{esign}{:02}", exp.abs());
    }
    if exp < 0 {
        return format!("{sign}0.{}{digits}", "0".repeat((-exp - 1) as usize));
    }
    let point = exp as usize + 1;
    if digits.len() <= point {
        format!("{sign}{digits}{}", "0".repeat(point - digits.len()))
    } else {
        let (int_part, frac) = digits.split_at(point);
        format!("{sign}{int_part}.{frac}")
    }
}

// ---------------------------------------------------------------------------
// Box operators.
//
// PostgreSQL's geometric comparisons are FUZZY: every one goes through the
// `FPeq` / `FPlt` / ... macros with `EPSILON 1.0E-06`, so two boxes whose
// corners differ by less than that are the same box, and `<<` needs a gap
// wider than it. A NaN coordinate fails every test, as it does in C.
// Measured on PostgreSQL 16.

const EPSILON: f64 = 1.0e-6;

fn fp_eq(a: f64, b: f64) -> bool {
    a == b || (a - b).abs() <= EPSILON
}
fn fp_lt(a: f64, b: f64) -> bool {
    (b - a) > EPSILON
}
fn fp_gt(a: f64, b: f64) -> bool {
    (a - b) > EPSILON
}
fn fp_le(a: f64, b: f64) -> bool {
    (b - a) >= -EPSILON
}
fn fp_ge(a: f64, b: f64) -> bool {
    (a - b) >= -EPSILON
}

/// PostgreSQL's own `pg_hypot`, which is not libm's: it scales by the larger
/// magnitude and takes one square root, and the last digit differs.
fn pg_hypot(x: f64, y: f64) -> f64 {
    if x.is_infinite() || y.is_infinite() {
        return f64::INFINITY;
    }
    if x.is_nan() || y.is_nan() {
        return f64::NAN;
    }
    let (mut x, mut y) = (x.abs(), y.abs());
    if x < y {
        std::mem::swap(&mut x, &mut y);
    }
    if x == 0.0 {
        return 0.0;
    }
    let yx = y / x;
    x * (1.0 + yx * yx).sqrt()
}

/// Whether an operator has a `box <op> box` form (`<>` included: it has
/// none, and the operator error names box on both sides).
pub fn is_box_operator(op: &str) -> bool {
    matches!(
        op,
        "=" | "<>"
            | "<"
            | "<="
            | ">"
            | ">="
            | "&&"
            | "@>"
            | "<@"
            | "<<"
            | ">>"
            | "&<"
            | "&>"
            | "<<|"
            | "|>>"
            | "&<|"
            | "|&>"
            | "~="
            | "<->"
            | "#"
    )
}

/// The result type of `box <op> box`.
pub fn box_operator_type(op: &str) -> &'static str {
    match op {
        "<->" => "float8",
        "#" => "box",
        _ => "bool",
    }
}

/// `box <op> box`. `a` and `b` are `[high.x, high.y, low.x, low.y]`.
pub fn box_operator(op: &str, a: &[f64; 4], b: &[f64; 4]) -> Result<Bson> {
    let (ahx, ahy, alx, aly) = (a[0], a[1], a[2], a[3]);
    let (bhx, bhy, blx, bly) = (b[0], b[1], b[2], b[3]);
    let area = |c: &[f64; 4]| (c[0] - c[2]) * (c[1] - c[3]);
    let overlaps = fp_ge(ahx, blx) && fp_ge(bhx, alx) && fp_ge(ahy, bly) && fp_ge(bhy, aly);
    let contains = |o: &[f64; 4], i: &[f64; 4]| {
        fp_ge(o[0], i[0]) && fp_le(o[2], i[2]) && fp_ge(o[1], i[1]) && fp_le(o[3], i[3])
    };
    Ok(match op {
        // Equality and ordering are by AREA, not by corners.
        "=" => Bson::Boolean(fp_eq(area(a), area(b))),
        "<" => Bson::Boolean(fp_lt(area(a), area(b))),
        "<=" => Bson::Boolean(fp_le(area(a), area(b))),
        ">" => Bson::Boolean(fp_gt(area(a), area(b))),
        ">=" => Bson::Boolean(fp_ge(area(a), area(b))),
        "~=" => {
            Bson::Boolean(fp_eq(ahx, bhx) && fp_eq(alx, blx) && fp_eq(ahy, bhy) && fp_eq(aly, bly))
        }
        "&&" => Bson::Boolean(overlaps),
        "@>" => Bson::Boolean(contains(a, b)),
        "<@" => Bson::Boolean(contains(b, a)),
        "<<" => Bson::Boolean(fp_lt(ahx, blx)),
        "&<" => Bson::Boolean(fp_le(ahx, bhx)),
        ">>" => Bson::Boolean(fp_gt(alx, bhx)),
        "&>" => Bson::Boolean(fp_ge(alx, blx)),
        "<<|" => Bson::Boolean(fp_lt(ahy, bly)),
        "&<|" => Bson::Boolean(fp_le(ahy, bhy)),
        "|>>" => Bson::Boolean(fp_gt(aly, bhy)),
        "|&>" => Bson::Boolean(fp_ge(aly, bly)),
        // The distance between the two centres.
        "<->" => {
            let (ax, ay) = ((ahx + alx) / 2.0, (ahy + aly) / 2.0);
            let (bx, by) = ((bhx + blx) / 2.0, (bhy + bly) / 2.0);
            Bson::Double(pg_hypot(ax - bx, ay - by))
        }
        // The intersection, or NULL when there is none.
        "#" => {
            if !overlaps {
                Bson::Null
            } else {
                box_value([ahx.min(bhx), ahy.min(bhy), alx.max(blx), aly.max(bly)])
            }
        }
        other => {
            return Err(Error::UndefinedFunction(format!(
                "operator does not exist: box {other} box"
            )))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float8_text_matches_float8out() {
        let cases = [
            (1e20, "1e+20"),
            (1e-7, "1e-07"),
            (0.1, "0.1"),
            (123456789012345680.0, "1.2345678901234568e+17"),
            (1e15, "1e+15"),
            (1e14, "100000000000000"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.5e300, "1.5e+300"),
            (-0.0, "-0"),
            (0.0, "0"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
            (12345.678, "12345.678"),
            (1e16, "1e+16"),
            (123456789012345.6, "123456789012345.6"),
            (400.0, "400"),
            (-2.5, "-2.5"),
            (1.0, "1"),
        ];
        for (v, want) in cases {
            assert_eq!(float8_text(v), want, "{v}");
        }
        assert_eq!(float8_text(f64::NAN), "NaN");
    }

    #[test]
    fn float4_text_matches_float4out() {
        let cases = [
            (0.1f32, "0.1"),
            (1.0 / 3.0, "0.33333334"),
            (16_777_217.0, "1.6777216e+07"),
            (123_456_789.0, "1.2345679e+08"),
            (1_000_000.0, "1e+06"),
            (999_999.0, "999999"),
            (100.5, "100.5"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1e20, "1e+20"),
            (-2.5, "-2.5"),
            (1.0, "1"),
        ];
        for (v, want) in cases {
            assert_eq!(float4_text(v), want, "{v}");
        }
        assert_eq!(float4_text(f32::NAN), "NaN");
        assert_eq!(float4_text(f32::INFINITY), "Infinity");
    }

    #[test]
    fn box_round_trips_as_postgresql_prints_it() {
        for (text, want) in [
            ("(1,2),(3,4)", "(3,4),(1,2)"),
            ("(3,4),(1,2)", "(3,4),(1,2)"),
            ("((1,2),(3,4))", "(3,4),(1,2)"),
            ("1,2,3,4", "(3,4),(1,2)"),
            ("(1,4),(3,2)", "(3,4),(1,2)"),
            ("(1.5,2),(3,4e2)", "(3,400),(1.5,2)"),
            (" ( 1 , 2 ) , ( 3 , 4 ) ", "(3,4),(1,2)"),
            ("(nan,1),(2,3)", "(NaN,3),(2,1)"),
            ("(2,3),(nan,1)", "(NaN,3),(2,1)"),
            ("(2,nan),(3,4)", "(3,NaN),(2,4)"),
            ("(nan,nan),(1,1)", "(NaN,NaN),(1,1)"),
            ("(+1,2),(3,4)", "(3,4),(1,2)"),
            ("(.5,2),(3,4)", "(3,4),(0.5,2)"),
            ("(inf,1),(2,3)", "(Infinity,3),(2,1)"),
            ("(-0,1),(2,3)", "(2,3),(-0,1)"),
        ] {
            assert_eq!(box_text(&parse_box(text).unwrap()), want, "{text}");
        }
        for bad in [
            "(1,2),(3)",
            "(1,2),(3,4),(5,6)",
            "x",
            "(1,2)",
            "((1,2),(3,4)",
        ] {
            let err = parse_box(bad).unwrap_err();
            assert_eq!(
                err.to_string(),
                format!("invalid input syntax for type box: \"{bad}\""),
                "{bad}"
            );
        }
        for (huge, token) in [("(1e400,1),(2,3)", "1e400"), ("(1e-400,1),(2,3)", "1e-400")] {
            assert_eq!(
                parse_box(huge).unwrap_err().to_string(),
                format!("\"{token}\" is out of range for type double precision")
            );
        }
    }
}
