//! `to_char` / `to_number` over numbers: PostgreSQL's `formatting.c`
//! numeric machinery, transcribed.
//!
//! The rules are too irregular to derive from examples -- `FM999.99` prints
//! `0.` for zero, `9.` prints `#.` for 12, `L` is a SPACE in the C locale --
//! so this follows `NUMDesc_prepare`, `NUM_processor`,
//! `NUM_numpart_to_char` / `NUM_numpart_from_char` and the per-type
//! `*_to_char` preparation step by step, with the same state variables. The
//! locale is PostgreSQL's C locale (the reference server's): decimal point
//! `.`, group separator `,`, currency symbol ` `.

use bson::Bson;

use crate::{Error, Result};

// ------------------------------------------------------------------------
// Format parsing
// ------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Comma,
    Dec,
    Zero,
    Nine,
    B,
    C,
    D,
    E,
    Fm,
    G,
    L,
    Mi,
    Pl,
    Pr,
    Rn,
    RnLower,
    Sg,
    Sp,
    S,
    Th,
    ThLower,
    V,
}

#[derive(Debug, Clone)]
enum Node {
    Action(Key),
    Char(String),
}

/// The keywords, longest first within each leading letter (`parse_format`
/// takes the first that matches).
const KEYWORDS: &[(&str, Key)] = &[
    (",", Key::Comma),
    (".", Key::Dec),
    ("0", Key::Zero),
    ("9", Key::Nine),
    ("B", Key::B),
    ("C", Key::C),
    ("D", Key::D),
    ("EEEE", Key::E),
    ("FM", Key::Fm),
    ("G", Key::G),
    ("L", Key::L),
    ("MI", Key::Mi),
    ("PL", Key::Pl),
    ("PR", Key::Pr),
    ("RN", Key::Rn),
    ("SG", Key::Sg),
    ("SP", Key::Sp),
    ("S", Key::S),
    ("TH", Key::Th),
    ("V", Key::V),
    ("b", Key::B),
    ("c", Key::C),
    ("d", Key::D),
    ("eeee", Key::E),
    ("fm", Key::Fm),
    ("g", Key::G),
    ("l", Key::L),
    ("mi", Key::Mi),
    ("pl", Key::Pl),
    ("pr", Key::Pr),
    ("rn", Key::RnLower),
    ("sg", Key::Sg),
    ("sp", Key::Sp),
    ("s", Key::S),
    ("th", Key::ThLower),
    ("v", Key::V),
];

fn parse_format(fmt: &str) -> Vec<Node> {
    let mut out = Vec::new();
    let mut rest = fmt;
    while !rest.is_empty() {
        if let Some((kw, key)) = KEYWORDS.iter().find(|(kw, _)| rest.starts_with(kw)) {
            out.push(Node::Action(*key));
            rest = &rest[kw.len()..];
            continue;
        }
        let c = rest.chars().next().expect("non-empty");
        if c == '"' {
            // A quoted literal: its characters, a `\"` inside it escaped.
            let mut chars = rest[1..].char_indices();
            let mut end = rest.len();
            let mut lit = String::new();
            while let Some((i, ch)) = chars.next() {
                if ch == '\\' {
                    if let Some((_, nx)) = chars.next() {
                        lit.push(nx);
                    }
                    continue;
                }
                if ch == '"' {
                    end = i + 2;
                    break;
                }
                lit.push(ch);
            }
            for ch in lit.chars() {
                out.push(Node::Char(ch.to_string()));
            }
            rest = &rest[end.min(rest.len())..];
            continue;
        }
        if c == '\\' && rest[1..].starts_with('"') {
            out.push(Node::Char("\"".into()));
            rest = &rest[2..];
            continue;
        }
        out.push(Node::Char(c.to_string()));
        rest = &rest[c.len_utf8()..];
    }
    out
}

// NUMDesc flags.
const F_DECIMAL: u32 = 1 << 1;
const F_LDECIMAL: u32 = 1 << 2;
const F_ZERO: u32 = 1 << 3;
const F_BLANK: u32 = 1 << 4;
const F_FILLMODE: u32 = 1 << 5;
const F_LSIGN: u32 = 1 << 6;
const F_BRACKET: u32 = 1 << 7;
const F_MINUS: u32 = 1 << 8;
const F_PLUS: u32 = 1 << 9;
const F_ROMAN: u32 = 1 << 10;
const F_MULTI: u32 = 1 << 11;
const F_PLUS_POST: u32 = 1 << 12;
const F_MINUS_POST: u32 = 1 << 13;
const F_EEEE: u32 = 1 << 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LSign {
    None,
    Pre,
    Post,
}

#[derive(Debug, Clone)]
struct NumDesc {
    pre: i32,
    post: i32,
    lsign: LSign,
    flag: u32,
    pre_lsign_num: i32,
    multi: i32,
    zero_start: i32,
    zero_end: i32,
}

impl NumDesc {
    fn has(&self, f: u32) -> bool {
        self.flag & f != 0
    }
}

fn fmt_err(msg: &str) -> Error {
    Error::Sqlstate("42601", msg.to_string())
}

fn prepare(nodes: &[Node]) -> Result<NumDesc> {
    let mut n = NumDesc {
        pre: 0,
        post: 0,
        lsign: LSign::None,
        flag: 0,
        pre_lsign_num: 0,
        multi: 0,
        zero_start: 0,
        zero_end: 0,
    };
    for node in nodes {
        let Node::Action(key) = node else { continue };
        if n.has(F_EEEE) && *key != Key::E {
            return Err(fmt_err("\"EEEE\" must be the last pattern used"));
        }
        match key {
            Key::Nine => {
                if n.has(F_BRACKET) {
                    return Err(fmt_err("\"9\" must be ahead of \"PR\""));
                }
                if n.has(F_MULTI) {
                    n.multi += 1;
                } else if n.has(F_DECIMAL) {
                    n.post += 1;
                } else {
                    n.pre += 1;
                }
            }
            Key::Zero => {
                if n.has(F_BRACKET) {
                    return Err(fmt_err("\"0\" must be ahead of \"PR\""));
                }
                if !n.has(F_ZERO) && !n.has(F_DECIMAL) {
                    n.flag |= F_ZERO;
                    n.zero_start = n.pre + 1;
                }
                if !n.has(F_DECIMAL) {
                    n.pre += 1;
                } else {
                    n.post += 1;
                }
                n.zero_end = n.pre + n.post;
            }
            Key::B => {
                if n.pre == 0 && n.post == 0 && !n.has(F_ZERO) {
                    n.flag |= F_BLANK;
                }
            }
            Key::D | Key::Dec => {
                if *key == Key::D {
                    n.flag |= F_LDECIMAL;
                }
                if n.has(F_DECIMAL) {
                    return Err(fmt_err("multiple decimal points"));
                }
                if n.has(F_MULTI) {
                    return Err(fmt_err("cannot use \"V\" and decimal point together"));
                }
                n.flag |= F_DECIMAL;
            }
            Key::Fm => n.flag |= F_FILLMODE,
            Key::S => {
                if n.has(F_LSIGN) {
                    return Err(fmt_err("cannot use \"S\" twice"));
                }
                if n.has(F_PLUS) || n.has(F_MINUS) || n.has(F_BRACKET) {
                    return Err(fmt_err(
                        "cannot use \"S\" and \"PL\"/\"MI\"/\"SG\"/\"PR\" together",
                    ));
                }
                if !n.has(F_DECIMAL) {
                    n.lsign = LSign::Pre;
                    n.pre_lsign_num = n.pre;
                    n.flag |= F_LSIGN;
                } else if n.lsign == LSign::None {
                    n.lsign = LSign::Post;
                    n.flag |= F_LSIGN;
                }
            }
            Key::Mi => {
                if n.has(F_LSIGN) {
                    return Err(fmt_err("cannot use \"S\" and \"MI\" together"));
                }
                n.flag |= F_MINUS;
                if n.has(F_DECIMAL) {
                    n.flag |= F_MINUS_POST;
                }
            }
            Key::Pl => {
                if n.has(F_LSIGN) {
                    return Err(fmt_err("cannot use \"S\" and \"PL\" together"));
                }
                n.flag |= F_PLUS;
                if n.has(F_DECIMAL) {
                    n.flag |= F_PLUS_POST;
                }
            }
            Key::Sg => {
                if n.has(F_LSIGN) {
                    return Err(fmt_err("cannot use \"S\" and \"SG\" together"));
                }
                n.flag |= F_MINUS | F_PLUS;
            }
            Key::Pr => {
                if n.has(F_LSIGN) || n.has(F_PLUS) || n.has(F_MINUS) {
                    return Err(fmt_err(
                        "cannot use \"PR\" and \"S\"/\"PL\"/\"MI\"/\"SG\" together",
                    ));
                }
                n.flag |= F_BRACKET;
            }
            Key::Rn | Key::RnLower => n.flag |= F_ROMAN,
            Key::V => {
                if n.has(F_DECIMAL) {
                    return Err(fmt_err("cannot use \"V\" and decimal point together"));
                }
                n.flag |= F_MULTI;
            }
            Key::E => {
                if n.has(F_EEEE) {
                    return Err(fmt_err("cannot use \"EEEE\" twice"));
                }
                if n.has(F_BLANK)
                    || n.has(F_FILLMODE)
                    || n.has(F_LSIGN)
                    || n.has(F_BRACKET)
                    || n.has(F_MINUS)
                    || n.has(F_PLUS)
                    || n.has(F_ROMAN)
                    || n.has(F_MULTI)
                {
                    return Err(fmt_err("\"EEEE\" is incompatible with other formats"));
                }
                n.flag |= F_EEEE;
            }
            _ => {}
        }
    }
    let _ = (F_PLUS_POST, F_MINUS_POST, F_LDECIMAL);
    Ok(n)
}

// ------------------------------------------------------------------------
// NUM_processor, to_char direction
// ------------------------------------------------------------------------

const DECIMAL: &str = ".";
const THOUSANDS: &str = ",";
const CURRENCY: &str = " ";

struct Proc<'a> {
    num: NumDesc,
    sign: char,
    sign_wrote: bool,
    num_count: i32,
    out_pre_spaces: i32,
    num_curr: i32,
    num_in: bool,
    /// The number's characters and the read position in them.
    number: &'a [u8],
    number_p: usize,
    /// Index into `number` of the last relevant decimal digit (FM).
    last_relevant: Option<usize>,
    out: String,
}

impl Proc<'_> {
    fn at(&self, i: usize) -> u8 {
        self.number.get(i).copied().unwrap_or(0)
    }

    fn predec_space(&self) -> bool {
        !self.num.has(F_ZERO) && self.number_p == 0 && self.at(0) == b'0' && self.num.post != 0
    }

    fn last_relevant_is_dot(&self) -> bool {
        self.last_relevant.is_some_and(|i| self.at(i) == b'.')
    }

    fn numpart(&mut self, key: Key) {
        if self.num.has(F_ROMAN) {
            return;
        }
        if !self.sign_wrote
            && (self.num_curr >= self.out_pre_spaces
                || (self.num.has(F_ZERO) && self.num.zero_start == self.num_curr))
            && (!self.predec_space() || self.last_relevant_is_dot())
        {
            if self.num.has(F_LSIGN) {
                if self.num.lsign == LSign::Pre {
                    self.out.push(if self.sign == '-' { '-' } else { '+' });
                    self.sign_wrote = true;
                }
            } else if self.num.has(F_BRACKET) {
                self.out.push(if self.sign == '+' { ' ' } else { '<' });
                self.sign_wrote = true;
            } else if self.sign == '+' {
                if !self.num.has(F_FILLMODE) {
                    self.out.push(' ');
                }
                self.sign_wrote = true;
            } else if self.sign == '-' {
                self.out.push('-');
                self.sign_wrote = true;
            }
        }

        if matches!(key, Key::Nine | Key::Zero | Key::D | Key::Dec) {
            if self.num_curr < self.out_pre_spaces
                && (self.num.zero_start > self.num_curr || !self.num.has(F_ZERO))
            {
                if !self.num.has(F_FILLMODE) {
                    self.out.push(' ');
                }
            } else if self.num.has(F_ZERO)
                && self.num_curr < self.out_pre_spaces
                && self.num.zero_start <= self.num_curr
            {
                self.out.push('0');
                self.num_in = true;
            } else {
                if self.at(self.number_p) == b'.' {
                    if !self.last_relevant_is_dot() || self.num.has(F_FILLMODE) {
                        self.out.push_str(DECIMAL);
                    }
                } else if self.last_relevant.is_some_and(|l| self.number_p > l) && key != Key::Zero
                {
                } else if self.predec_space() {
                    if !self.num.has(F_FILLMODE) {
                        self.out.push(' ');
                    } else if self.last_relevant_is_dot() {
                        self.out.push('0');
                    }
                } else {
                    let c = self.at(self.number_p);
                    if c != 0 {
                        self.out.push(c as char);
                    }
                    self.num_in = true;
                }
                if self.at(self.number_p) != 0 {
                    self.number_p += 1;
                }
            }

            let mut end = self.num_count
                + i32::from(self.out_pre_spaces != 0)
                + i32::from(self.num.has(F_DECIMAL));
            if self.last_relevant.is_some_and(|l| l == self.number_p) {
                end = self.num_curr;
            }
            if self.num_curr + 1 == end {
                if self.sign_wrote && self.num.has(F_BRACKET) {
                    self.out.push(if self.sign == '+' { ' ' } else { '>' });
                } else if self.num.has(F_LSIGN) && self.num.lsign == LSign::Post {
                    self.out.push(if self.sign == '-' { '-' } else { '+' });
                }
            }
        }
        self.num_curr += 1;
    }
}

/// The ordinal suffix of a number's text (`get_th`).
fn ordinal(number: &str, upper: bool) -> String {
    let b = number.as_bytes();
    let mut last = b.last().copied().unwrap_or(b'0');
    if b.len() > 1 && b[b.len() - 2] == b'1' {
        last = 0;
    }
    let s = match last {
        b'1' => "st",
        b'2' => "nd",
        b'3' => "rd",
        _ => "th",
    };
    if upper {
        s.to_ascii_uppercase()
    } else {
        s.to_string()
    }
}

/// Run the format over a prepared number string (`NUM_processor`, to_char).
fn process(
    nodes: &[Node],
    mut num: NumDesc,
    number: &str,
    out_pre_spaces: i32,
    sign: char,
) -> String {
    if num.zero_start != 0 {
        num.zero_start -= 1;
    }
    if num.has(F_EEEE) {
        return number.to_string();
    }
    let mut out_pre_spaces = out_pre_spaces;
    let mut sign = sign;
    if num.has(F_ROMAN) {
        num.lsign = LSign::None;
        num.pre_lsign_num = 0;
        num.post = 0;
        num.pre = 0;
        out_pre_spaces = 0;
        sign = '\0';
    }
    let sign_wrote;
    if num.has(F_PLUS) || num.has(F_MINUS) {
        sign_wrote = !(num.has(F_PLUS) && !num.has(F_MINUS));
    } else {
        if sign != '-' && num.has(F_FILLMODE) {
            num.flag &= !F_BRACKET;
        }
        sign_wrote = sign == '+' && num.has(F_FILLMODE) && !num.has(F_LSIGN);
        if num.lsign == LSign::Pre && num.pre == num.pre_lsign_num {
            num.lsign = LSign::Post;
        }
    }
    let bytes = number.as_bytes();
    let mut p = Proc {
        num,
        sign,
        sign_wrote,
        num_count: 0,
        out_pre_spaces,
        num_curr: 0,
        num_in: false,
        number: bytes,
        number_p: 0,
        last_relevant: None,
        out: String::new(),
    };
    p.num_count = p.num.post + p.num.pre - 1;
    if p.num.has(F_FILLMODE) && p.num.has(F_DECIMAL) {
        // get_last_relevant_decnum
        if let Some(dot) = number.find('.') {
            let mut r = dot;
            for (i, c) in bytes.iter().enumerate().skip(dot + 1) {
                if *c != b'0' {
                    r = i;
                }
            }
            p.last_relevant = Some(r);
            if p.num.zero_end > p.out_pre_spaces {
                let last_zero = (p.num.zero_end - p.out_pre_spaces) as usize;
                if r < last_zero {
                    p.last_relevant = Some(last_zero);
                }
            }
        }
    }
    if !p.sign_wrote && p.out_pre_spaces == 0 {
        p.num_count += 1;
    }

    for node in nodes {
        match node {
            Node::Char(c) => p.out.push_str(c),
            Node::Action(key) => match key {
                Key::Nine | Key::Zero | Key::Dec | Key::D => p.numpart(*key),
                Key::Comma | Key::G => {
                    if !p.num_in {
                        if !p.num.has(F_FILLMODE) {
                            p.out.push(' ');
                        }
                    } else {
                        p.out.push_str(THOUSANDS);
                    }
                }
                Key::L => p.out.push_str(CURRENCY),
                Key::Rn | Key::RnLower => {
                    let r = if *key == Key::Rn {
                        number.to_string()
                    } else {
                        number.to_ascii_lowercase()
                    };
                    if p.num.has(F_FILLMODE) {
                        p.out.push_str(&r);
                    } else {
                        p.out.push_str(&format!("{r:>15}"));
                    }
                }
                Key::Th | Key::ThLower => {
                    if p.num.has(F_ROMAN)
                        || number.starts_with('#')
                        || p.sign == '-'
                        || p.num.has(F_DECIMAL)
                    {
                        continue;
                    }
                    p.out.push_str(&ordinal(number, *key == Key::Th));
                }
                Key::Mi => {
                    if p.sign == '-' {
                        p.out.push('-');
                    } else if !p.num.has(F_FILLMODE) {
                        p.out.push(' ');
                    }
                }
                Key::Pl => {
                    if p.sign == '+' {
                        p.out.push('+');
                    } else if !p.num.has(F_FILLMODE) {
                        p.out.push(' ');
                    }
                }
                Key::Sg => {
                    if p.sign != '\0' {
                        p.out.push(p.sign);
                    }
                }
                _ => {}
            },
        }
    }
    p.out
}

// ------------------------------------------------------------------------
// Number preparation per type
// ------------------------------------------------------------------------

fn roman(n: i64) -> String {
    if !(1..=3999).contains(&n) {
        return "#".repeat(15);
    }
    const TABLE: &[(i64, &str)] = &[
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut n = n;
    let mut s = String::new();
    for (v, r) in TABLE {
        while n >= *v {
            s.push_str(r);
            n -= v;
        }
    }
    s
}

/// A decimal number as sign, integer digits and fraction digits.
#[derive(Debug, Clone)]
struct Dec {
    neg: bool,
    int: String,
    frac: String,
}

fn parse_dec(text: &str) -> Option<Dec> {
    let t = text.trim();
    let (neg, body) = match t.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let (mant, exp) = match body.find(['e', 'E']) {
        Some(i) => (&body[..i], body[i + 1..].parse::<i64>().ok()?),
        None => (body, 0),
    };
    let (i, f) = mant.split_once('.').unwrap_or((mant, ""));
    if !i.chars().chain(f.chars()).all(|c| c.is_ascii_digit()) || (i.is_empty() && f.is_empty()) {
        return None;
    }
    let mut digits: String = format!("{i}{f}");
    let mut point = i.len() as i64 + exp;
    if point < 0 {
        digits = "0".repeat((-point) as usize) + &digits;
        point = 0;
    }
    while (digits.len() as i64) < point {
        digits.push('0');
    }
    let (int, frac) = digits.split_at(point as usize);
    let int = int.trim_start_matches('0');
    Some(Dec {
        neg,
        int: if int.is_empty() {
            "0".into()
        } else {
            int.to_string()
        },
        frac: frac.to_string(),
    })
}

/// Round to `scale` fraction digits, half away from zero (numeric_round).
fn round_dec(d: &Dec, scale: usize) -> Dec {
    let mut frac: Vec<u8> = d.frac.bytes().collect();
    let mut int: Vec<u8> = d.int.bytes().collect();
    let round_up = frac.len() > scale && frac[scale] >= b'5';
    frac.truncate(scale);
    while frac.len() < scale {
        frac.push(b'0');
    }
    if round_up {
        let mut carry = true;
        for c in frac.iter_mut().rev() {
            if *c == b'9' {
                *c = b'0';
            } else {
                *c += 1;
                carry = false;
                break;
            }
        }
        if carry {
            for c in int.iter_mut().rev() {
                if *c == b'9' {
                    *c = b'0';
                } else {
                    *c += 1;
                    carry = false;
                    break;
                }
            }
            if carry {
                int.insert(0, b'1');
            }
        }
    }
    let int = String::from_utf8(int).unwrap_or_default();
    let frac = String::from_utf8(frac).unwrap_or_default();
    let zero = int.bytes().all(|c| c == b'0') && frac.bytes().all(|c| c == b'0');
    Dec {
        neg: d.neg && !zero,
        int,
        frac,
    }
}

fn dec_text(d: &Dec) -> String {
    if d.frac.is_empty() {
        d.int.clone()
    } else {
        format!("{}.{}", d.int, d.frac)
    }
}

/// `numeric_out_sci`: `d.ddde+XX`.
fn sci_numeric(d: &Dec, scale: usize) -> String {
    let digits = format!("{}{}", d.int, d.frac);
    let first = digits.find(|c: char| c != '0');
    let (exp, sig_digits) = match first {
        None => (0i64, "0".to_string()),
        Some(i) => (d.int.len() as i64 - 1 - i as i64, digits[i..].to_string()),
    };
    let sig = Dec {
        neg: false,
        int: sig_digits[..1].to_string(),
        frac: sig_digits[1..].to_string(),
    };
    let mut r = round_dec(&sig, scale);
    let mut exp = exp;
    if r.int.len() > 1 {
        // 9.99 rounded up to 10.0: one more power of ten.
        r = Dec {
            neg: false,
            int: r.int[..1].to_string(),
            frac: format!("{}{}", &r.int[1..], r.frac)[..scale].to_string(),
        };
        exp += 1;
    }
    let sign = if d.neg && first.is_some() { "-" } else { "" };
    format!(
        "{sign}{}e{}{:02}",
        dec_text(&r),
        if exp < 0 { '-' } else { '+' },
        exp.abs()
    )
}

/// C's `%+.*e` with the leading `+` swapped for a space.
fn sci_float(v: f64, scale: usize) -> String {
    let s = format!("{:+.*e}", scale, v);
    let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let e: i32 = exp.parse().unwrap_or(0);
    let mut out = format!("{mant}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs());
    if out.starts_with('+') {
        out.replace_range(..1, " ");
    }
    out
}

/// The value a to_char argument carries.
pub enum NumArg {
    Int(i64),
    Float(f64, bool),
    Numeric(String),
}

/// `to_char(number, format)`.
pub fn to_char_number(value: NumArg, fmt: &str) -> Result<String> {
    if fmt.is_empty() {
        return Ok(String::new());
    }
    let nodes = parse_format(fmt);
    let mut num = prepare(&nodes)?;
    let mut out_pre_spaces = 0;
    let mut sign = '+';
    let numstr: String;
    let overflow = |num: &NumDesc| -> String {
        let mut s = "#".repeat((num.pre + num.post + 1) as usize);
        if (num.pre as usize) < s.len() {
            s.replace_range(num.pre as usize..num.pre as usize + 1, ".");
        }
        s
    };
    if num.has(F_ROMAN) {
        let n = match &value {
            NumArg::Int(i) => *i,
            NumArg::Float(f, _) => f.round_ties_even() as i64,
            NumArg::Numeric(t) => {
                let d = round_dec(&parse_dec(t).ok_or_else(|| bad_num(t))?, 0);
                let v: i64 = d.int.parse().unwrap_or(i64::MAX);
                if d.neg {
                    -v
                } else {
                    v
                }
            }
        };
        numstr = roman(n);
    } else if num.has(F_EEEE) {
        numstr = match &value {
            NumArg::Float(f, _) if f.is_nan() || f.is_infinite() => {
                let mut s = "#".repeat((num.pre + num.post + 6) as usize);
                s.replace_range(..1, " ");
                let dot = (num.pre + 1) as usize;
                if dot < s.len() {
                    s.replace_range(dot..dot + 1, ".");
                }
                s
            }
            NumArg::Float(f, _) => sci_float(*f, num.post as usize),
            NumArg::Int(i) => sci_float(*i as f64, num.post as usize),
            NumArg::Numeric(t) => {
                let d = parse_dec(t).ok_or_else(|| bad_num(t))?;
                let s = sci_numeric(&d, num.post as usize);
                if s.starts_with('-') {
                    s
                } else {
                    format!(" {s}")
                }
            }
        };
    } else {
        let text = match &value {
            NumArg::Int(i) => {
                let v = if num.has(F_MULTI) {
                    i.saturating_mul(10i64.saturating_pow(num.multi as u32))
                } else {
                    *i
                };
                if num.has(F_MULTI) {
                    num.pre += num.multi;
                }
                let mut s = v.to_string();
                if num.post > 0 {
                    s.push('.');
                    s.push_str(&"0".repeat(num.post as usize));
                }
                s
            }
            NumArg::Float(f, single) => {
                let mut v = *f;
                if num.has(F_MULTI) {
                    v *= 10f64.powi(num.multi);
                    num.pre += num.multi;
                }
                let dig = if *single { 6 } else { 15 };
                let pre_len = format!("{:.0}", v.abs()).len() as i32;
                if pre_len >= dig {
                    num.post = 0;
                } else if pre_len + num.post > dig {
                    num.post = dig - pre_len;
                }
                format!("{:.*}", num.post as usize, v)
            }
            NumArg::Numeric(t) => {
                let mut d = parse_dec(t).ok_or_else(|| bad_num(t))?;
                if num.has(F_MULTI) {
                    d = shift_dec(&d, num.multi);
                    num.pre += num.multi;
                }
                let r = round_dec(&d, num.post.max(0) as usize);
                format!("{}{}", if r.neg { "-" } else { "" }, dec_text(&r))
            }
        };
        let body = match text.strip_prefix('-') {
            Some(b) => {
                sign = '-';
                b.to_string()
            }
            None => text,
        };
        let pre_len = body.find('.').unwrap_or(body.len()) as i32;
        if pre_len < num.pre {
            out_pre_spaces = num.pre - pre_len;
            numstr = body;
        } else if pre_len > num.pre {
            numstr = overflow(&num);
        } else {
            numstr = body;
        }
    }
    Ok(process(&nodes, num, &numstr, out_pre_spaces, sign))
}

fn shift_dec(d: &Dec, by: i32) -> Dec {
    let text = format!("{}{}e{by}", if d.neg { "-" } else { "" }, dec_text(d));
    parse_dec(&text).unwrap_or_else(|| d.clone())
}

fn bad_num(t: &str) -> Error {
    Error::InvalidText(format!("invalid input syntax for type numeric: \"{t}\""))
}

// ------------------------------------------------------------------------
// to_number
// ------------------------------------------------------------------------

fn utf8_len(b: &[u8], i: usize) -> usize {
    std::str::from_utf8(&b[i..])
        .ok()
        .and_then(|s| s.chars().next())
        .map_or(1, char::len_utf8)
}

/// `NUM_processor`'s reading state.
struct FromChar<'a> {
    num: &'a NumDesc,
    inp: &'a [u8],
    ip: usize,
    /// `number[0]` is the sign slot.
    number: Vec<u8>,
    read_pre: i32,
    read_post: i32,
    read_dec: bool,
}

impl FromChar<'_> {
    fn overload(&self) -> bool {
        self.ip >= self.inp.len()
    }

    fn amount(&self, n: usize) -> bool {
        self.ip + n <= self.inp.len()
    }

    /// `NUM_eat_non_data_chars`.
    fn eat_non_data(&mut self, n: usize) {
        for _ in 0..n {
            if self.overload() || b"0123456789.,+-".contains(&self.inp[self.ip]) {
                break;
            }
            self.ip += utf8_len(self.inp, self.ip);
        }
    }

    /// `NUM_numpart_from_char`.
    fn numpart(&mut self, key: Key) {
        let num = self.num;
        if self.overload() {
            return;
        }
        if self.inp[self.ip] == b' ' {
            self.ip += 1;
        }
        if self.overload() {
            return;
        }
        if self.number[0] == b' '
            && matches!(key, Key::Zero | Key::Nine)
            && self.read_pre + self.read_post == 0
        {
            let c = self.inp[self.ip];
            if num.has(F_LSIGN) && num.lsign == LSign::Pre {
                if self.amount(1) && c == b'-' {
                    self.ip += 1;
                    self.number[0] = b'-';
                } else if self.amount(1) && c == b'+' {
                    self.ip += 1;
                    self.number[0] = b'+';
                }
            } else if c == b'-' || (num.has(F_BRACKET) && c == b'<') {
                self.number[0] = b'-';
                self.ip += 1;
            } else if c == b'+' {
                self.number[0] = b'+';
                self.ip += 1;
            }
        }
        if self.overload() {
            return;
        }
        let mut isread = false;
        let c = self.inp[self.ip];
        if c.is_ascii_digit() {
            if self.read_dec && self.read_post == num.post {
                return;
            }
            self.number.push(c);
            if self.read_dec {
                self.read_post += 1;
            } else {
                self.read_pre += 1;
            }
            isread = true;
        } else if num.has(F_DECIMAL) && !self.read_dec && c == b'.' {
            self.number.push(b'.');
            self.read_dec = true;
            isread = true;
        }
        if self.overload() {
            return;
        }
        if self.number[0] == b' ' && self.read_pre + self.read_post > 0 {
            let len = self.inp.len();
            if num.has(F_LSIGN)
                && isread
                && self.ip + 1 < len
                && !self.inp[self.ip + 1].is_ascii_digit()
            {
                let save = self.ip;
                self.ip += 1;
                match self.inp[self.ip] {
                    b'-' => self.number[0] = b'-',
                    b'+' => self.number[0] = b'+',
                    _ => {}
                }
                if self.number[0] == b' ' {
                    self.ip = save;
                }
            } else if !isread
                && !num.has(F_LSIGN)
                && (num.has(F_PLUS) || num.has(F_MINUS))
                && matches!(self.inp[self.ip], b'-' | b'+')
            {
                self.number[0] = self.inp[self.ip];
            }
        }
    }
}

/// `to_number(text, format)`: `None` for an empty format (PostgreSQL
/// returns NULL). The result is the numeric text, already fitted to the
/// format's precision and scale.
pub fn to_number(input: &str, fmt: &str) -> Result<Option<String>> {
    if fmt.is_empty() {
        return Ok(None);
    }
    let nodes = parse_format(fmt);
    let mut num = prepare(&nodes)?;
    if num.has(F_ROMAN) {
        return Err(Error::Sqlstate(
            "0A000",
            "\"RN\" not supported for input".into(),
        ));
    }
    if num.has(F_EEEE) {
        return Err(Error::Sqlstate(
            "0A000",
            "\"EEEE\" not supported for input".into(),
        ));
    }
    if num.zero_start != 0 {
        num.zero_start -= 1;
    }
    let inp = input.as_bytes();
    let len = inp.len();
    let mut st = FromChar {
        num: &num,
        inp,
        ip: 0,
        number: vec![b' '],
        read_pre: 0,
        read_post: 0,
        read_dec: false,
    };
    for node in &nodes {
        if st.ip >= len {
            break;
        }
        match node {
            Node::Char(_) => {
                // One input character skipped per literal, matching or not.
                st.ip += utf8_len(inp, st.ip);
                continue;
            }
            Node::Action(key) => match key {
                Key::Nine | Key::Zero | Key::Dec | Key::D => st.numpart(*key),
                // `num_in` is never set while reading, so a fill-mode
                // format skips every separator.
                Key::Comma => {
                    if num.has(F_FILLMODE) || inp[st.ip] != b',' {
                        continue;
                    }
                }
                Key::G => {
                    if num.has(F_FILLMODE) || !inp[st.ip..].starts_with(THOUSANDS.as_bytes()) {
                        continue;
                    }
                    st.ip += THOUSANDS.len() - 1;
                }
                Key::L => {
                    st.eat_non_data(CURRENCY.chars().count());
                    continue;
                }
                Key::Th | Key::ThLower => {
                    if num.has(F_ROMAN) || st.number.first() == Some(&b'#') || num.has(F_DECIMAL) {
                        continue;
                    }
                    st.eat_non_data(2);
                    continue;
                }
                Key::Mi | Key::Pl | Key::Sg => {
                    let c = inp[st.ip];
                    let takes = match key {
                        Key::Mi => c == b'-',
                        Key::Pl => c == b'+',
                        _ => c == b'-' || c == b'+',
                    };
                    if takes {
                        st.number[0] = c;
                    } else {
                        st.eat_non_data(1);
                        continue;
                    }
                }
                _ => continue,
            },
        }
        st.ip += 1;
    }
    let mut number = st.number;
    let read_post = st.read_post;
    if number.last() == Some(&b'.') {
        number.pop();
    }
    let text = String::from_utf8(number).unwrap_or_default();
    let scale = read_post;
    let precision = num.pre + num.multi + scale;
    // numeric_in with the typmod (precision, scale).
    let trimmed = text.trim();
    let d = parse_dec(trimmed.trim_start_matches('+')).ok_or_else(|| {
        Error::InvalidText(format!("invalid input syntax for type numeric: \"{text}\""))
    })?;
    let mut r = round_dec(&d, scale.max(0) as usize);
    if trimmed.starts_with('-') {
        r.neg = !(r.int.bytes().all(|c| c == b'0') && r.frac.bytes().all(|c| c == b'0'));
    }
    let int_digits = r.int.trim_start_matches('0').len() as i32;
    if int_digits > precision - scale {
        return Err(Error::Sqlstate("22003", "numeric field overflow".into()));
    }
    let mut out = format!("{}{}", if r.neg { "-" } else { "" }, dec_text(&r));
    if num.has(F_MULTI) {
        // `result * power(10, -multi)`: the power is computed at scale 16
        // (so past 16 digits it is zero), and a product's scale is the sum.
        let scale = scale.max(0) as usize + 16;
        let d = if num.multi > 16 {
            Dec {
                neg: false,
                int: "0".into(),
                frac: String::new(),
            }
        } else {
            shift_dec(&parse_dec(&out).expect("valid"), -num.multi)
        };
        let mut frac = d.frac.clone();
        frac.truncate(scale);
        while frac.len() < scale {
            frac.push('0');
        }
        let zero = d.int.bytes().all(|c| c == b'0') && frac.bytes().all(|c| c == b'0');
        out = format!("{}{}.{frac}", if d.neg && !zero { "-" } else { "" }, d.int);
    }
    Ok(Some(out))
}

/// `to_char` / `to_number` over SQL values, or `None` when the first
/// argument is not a number (the datetime forms live elsewhere).
pub fn to_char_value(v: &Bson, fmt: &str, float4: bool) -> Option<Result<String>> {
    let arg = match v {
        Bson::Int32(i) => NumArg::Int(i64::from(*i)),
        Bson::Int64(i) => NumArg::Int(*i),
        Bson::Double(f) => NumArg::Float(*f, float4),
        Bson::Decimal128(_) => NumArg::Numeric(crate::value_text(v)),
        Bson::Document(d) if d.contains_key(crate::WIDE_NUMERIC_KEY) => {
            NumArg::Numeric(crate::value_text(v))
        }
        _ => return None,
    };
    Some(to_char_number(arg, fmt))
}
