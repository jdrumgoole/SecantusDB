//! SQL/JSON path: the `jsonpath` type and `jsonb_path_*` / `@?` / `@@`.
//!
//! Parser and printer follow PostgreSQL 14's `jsonpath_gram.y` and
//! `printJsonPathItem`; the executor transcribes `jsonpath_exec.c` function
//! for function -- `executeItemOptUnwrapTarget`, `executePredicate`,
//! `compareItems`, the lax/strict wrap and unwrap rules, and the
//! three-valued predicate logic -- because the behaviour lives in those
//! details: lax mode unwraps an array under `.key` and wraps a scalar under
//! `[*]`, a comparison across types is UNKNOWN (so a filter drops it) except
//! against `null`, and every structural error is swallowed in lax mode but
//! raised in strict.
//!
//! `.datetime()` parses an ISO form (or a template's) into a date / time /
//! timetz / timestamp / timestamptz item, compared as that kind -- and a
//! comparison that needs a time zone is refused unless a `*_tz` function
//! asked for one, as PostgreSQL has it.
//!
//! Not implemented: `.keyvalue()`'s `id` for a nested object (PostgreSQL derives it from the
//! object's byte offset inside the binary jsonb, which this server does not
//! store).

use crate::json::Json;
use crate::{Error, Result};

// ------------------------------------------------------------------------
// The path tree
// ------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    And,
    Or,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    StartsWith,
}

impl Op {
    fn name(self) -> &'static str {
        match self {
            Op::And => "&&",
            Op::Or => "||",
            Op::Eq => "==",
            Op::Ne => "!=",
            Op::Lt => "<",
            Op::Gt => ">",
            Op::Le => "<=",
            Op::Ge => ">=",
            Op::Add => "+",
            Op::Sub => "-",
            Op::Mul => "*",
            Op::Div => "/",
            Op::Mod => "%",
            Op::StartsWith => "starts with",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Type,
    Size,
    Double,
    Abs,
    Floor,
    Ceiling,
    KeyValue,
}

impl Method {
    fn name(self) -> &'static str {
        match self {
            Method::Type => "type",
            Method::Size => "size",
            Method::Double => "double",
            Method::Abs => "abs",
            Method::Floor => "floor",
            Method::Ceiling => "ceiling",
            Method::KeyValue => "keyvalue",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Null,
    Bool(bool),
    Num(String),
    Str(String),
    Var(String),
    Root,
    Current,
    Last,
    Key(String),
    AnyKey,
    AnyArray,
    /// `[a, b to c]`.
    Index(Vec<(Node, Option<Node>)>),
    /// `.**{first to last}`; `u32::MAX` is `last`.
    Any(u32, u32),
    Filter(Box<Node>),
    Method(Method),
    Datetime(Option<String>),
    Binary(Op, Box<Node>, Box<Node>),
    Plus(Box<Node>),
    Minus(Box<Node>),
    Not(Box<Node>),
    IsUnknown(Box<Node>),
    Exists(Box<Node>),
    LikeRegex(Box<Node>, String, String),
}

/// One item and the rest of its accessor chain.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub item: Item,
    pub next: Option<Box<Node>>,
}

impl Node {
    fn new(item: Item) -> Node {
        Node { item, next: None }
    }

    fn append(&mut self, tail: Node) {
        match &mut self.next {
            Some(n) => n.append(tail),
            None => self.next = Some(Box::new(tail)),
        }
    }

    fn is_predicate(&self) -> bool {
        self.next.is_none()
            && matches!(
                &self.item,
                Item::Binary(
                    Op::And
                        | Op::Or
                        | Op::Eq
                        | Op::Ne
                        | Op::Lt
                        | Op::Gt
                        | Op::Le
                        | Op::Ge
                        | Op::StartsWith,
                    _,
                    _
                ) | Item::Not(_)
                    | Item::IsUnknown(_)
                    | Item::Exists(_)
                    | Item::LikeRegex(..)
            )
    }
}

/// A parsed path: its mode and expression (`None`: the empty path).
#[derive(Debug, Clone, PartialEq)]
pub struct JsonPath {
    pub lax: bool,
    pub expr: Option<Node>,
}

// ------------------------------------------------------------------------
// Lexer
// ------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Str(String),
    Var(String),
    Ident(String),
    Num(String),
    Int(String),
    Punct(&'static str),
}

fn syntax(input: &str, what: &str) -> Error {
    Error::Sqlstate(
        "42601",
        format!(
            "syntax error, {what} at or near \"{}\" of jsonpath input",
            input.chars().take(40).collect::<String>()
        ),
    )
}

fn is_special(c: char) -> bool {
    "?%$.[]{}()|&!=<>@#,*:-+/\\\" \t\n\r\x0c".contains(c)
}

/// Tokens with their source text, for error messages.
fn lex_raw(input: &str) -> Result<Vec<(Tok, String)>> {
    let chars: Vec<char> = input.chars().collect();
    let n = chars.len();
    let mut i = 0;
    let mut out: Vec<Tok> = Vec::new();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let read_escaped = |i: &mut usize, end_quote: bool| -> Result<String> {
        let mut s = String::new();
        loop {
            if *i >= n {
                if end_quote {
                    return Err(syntax(input, "unexpected end of quoted string"));
                }
                return Ok(s);
            }
            let c = chars[*i];
            if end_quote && c == '"' {
                *i += 1;
                return Ok(s);
            }
            if !end_quote && (is_special(c) && c != '\\') {
                return Ok(s);
            }
            if c == '\\' {
                *i += 1;
                if *i >= n {
                    return Err(syntax(input, "unexpected end after backslash"));
                }
                let e = chars[*i];
                *i += 1;
                match e {
                    'b' => s.push('\u{8}'),
                    'f' => s.push('\u{c}'),
                    'n' => s.push('\n'),
                    'r' => s.push('\r'),
                    't' => s.push('\t'),
                    'v' => s.push('\u{b}'),
                    'x' => {
                        let h: String = chars[*i..(*i + 2).min(n)].iter().collect();
                        let v = u32::from_str_radix(&h, 16)
                            .map_err(|_| syntax(input, "invalid hex character sequence"))?;
                        s.push(char::from_u32(v).unwrap_or('\u{fffd}'));
                        *i += 2;
                    }
                    'u' => {
                        let (v, used) = if *i < n && chars[*i] == '{' {
                            let end = chars[*i..].iter().position(|c| *c == '}').map(|p| *i + p);
                            let end =
                                end.ok_or_else(|| syntax(input, "invalid unicode sequence"))?;
                            let h: String = chars[*i + 1..end].iter().collect();
                            (u32::from_str_radix(&h, 16).ok(), end + 1 - *i)
                        } else {
                            let h: String = chars[*i..(*i + 4).min(n)].iter().collect();
                            (u32::from_str_radix(&h, 16).ok(), 4)
                        };
                        let v = v.ok_or_else(|| syntax(input, "invalid unicode sequence"))?;
                        *i += used;
                        s.push(char::from_u32(v).unwrap_or('\u{fffd}'));
                    }
                    other => s.push(other),
                }
                continue;
            }
            s.push(c);
            *i += 1;
        }
    };
    while i < n {
        let start = i;
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '/' && i + 1 < n && chars[i + 1] == '*' {
            let rest: String = chars[i + 2..].iter().collect();
            match rest.find("*/") {
                Some(p) => i += 2 + rest[..p].chars().count() + 2,
                None => return Err(syntax(input, "unexpected end of comment")),
            }
            continue;
        }
        let two: String = chars[i..(i + 2).min(n)].iter().collect();
        if let Some(p) = ["&&", "||", "**", "<=", "==", "<>", "!=", ">="]
            .iter()
            .find(|p| two == **p)
        {
            push_tok(&mut out, &mut spans, start, Tok::Punct(p));
            i += 2;
            continue;
        }
        if c == '"' {
            i += 1;
            push_tok(
                &mut out,
                &mut spans,
                start,
                Tok::Str(read_escaped(&mut i, true)?),
            );
            continue;
        }
        if c == '$' {
            if i + 1 < n && chars[i + 1] == '"' {
                i += 2;
                push_tok(
                    &mut out,
                    &mut spans,
                    start,
                    Tok::Var(read_escaped(&mut i, true)?),
                );
                continue;
            }
            if i + 1 < n && !is_special(chars[i + 1]) {
                i += 1;
                push_tok(
                    &mut out,
                    &mut spans,
                    start,
                    Tok::Var(read_escaped(&mut i, false)?),
                );
                continue;
            }
            push_tok(&mut out, &mut spans, start, Tok::Punct("$"));
            i += 1;
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < n && chars[i].is_ascii_digit() {
                i += 1;
            }
            let mut real = false;
            if i < n && chars[i] == '.' && i + 1 < n && chars[i + 1].is_ascii_digit() {
                i += 1;
                while i < n && chars[i].is_ascii_digit() {
                    i += 1;
                }
                real = true;
            }
            if i < n && (chars[i] == 'e' || chars[i] == 'E') {
                let mut j = i + 1;
                if j < n && (chars[j] == '+' || chars[j] == '-') {
                    j += 1;
                }
                if j < n && chars[j].is_ascii_digit() {
                    while j < n && chars[j].is_ascii_digit() {
                        j += 1;
                    }
                    i = j;
                    real = true;
                } else {
                    return Err(syntax(input, "invalid floating point number"));
                }
            }
            if i < n && !is_special(chars[i]) && !chars[i].is_ascii_digit() {
                return Err(syntax(input, "trailing junk after numeric literal"));
            }
            let text: String = chars[start..i].iter().collect();
            push_tok(
                &mut out,
                &mut spans,
                start,
                if real { Tok::Num(text) } else { Tok::Int(text) },
            );
            continue;
        }
        if is_special(c) && c != '\\' {
            const PUNCT: &[&str] = &[
                "?", "%", "$", ".", "[", "]", "{", "}", "(", ")", "|", "&", "!", "=", "<", ">",
                "@", "#", ",", "*", ":", "-", "+", "/",
            ];
            let s = c.to_string();
            let p = PUNCT
                .iter()
                .find(|p| **p == s)
                .ok_or_else(|| syntax(input, "unexpected character"))?;
            push_tok(&mut out, &mut spans, start, Tok::Punct(p));
            i += 1;
            continue;
        }
        push_tok(
            &mut out,
            &mut spans,
            start,
            Tok::Ident(read_escaped(&mut i, false)?),
        );
    }
    let raws: Vec<String> = spans
        .iter()
        .enumerate()
        .map(|(k, (a, _))| {
            let end = spans.get(k + 1).map_or(n, |(b, _)| *b);
            chars[*a..end].iter().collect::<String>().trim().to_string()
        })
        .collect();
    Ok(out.into_iter().zip(raws).collect())
}

fn push_tok(out: &mut Vec<Tok>, spans: &mut Vec<(usize, usize)>, start: usize, t: Tok) {
    out.push(t);
    spans.push((start, start));
}

// ------------------------------------------------------------------------
// Parser
// ------------------------------------------------------------------------

struct Parser<'a> {
    toks: Vec<Tok>,
    raws: Vec<String>,
    pos: usize,
    input: &'a str,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn peek_at(&self, k: usize) -> Option<&Tok> {
        self.toks.get(self.pos + k)
    }

    fn is_punct(&self, p: &str) -> bool {
        matches!(self.peek(), Some(Tok::Punct(q)) if *q == p)
    }

    fn is_kw(&self, k: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(s)) if s == k)
    }

    fn eat_punct(&mut self, p: &str) -> bool {
        if self.is_punct(p) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect_punct(&mut self, p: &str) -> Result<()> {
        if self.eat_punct(p) {
            Ok(())
        } else {
            Err(self.err())
        }
    }

    fn err(&self) -> Error {
        let _ = self.input;
        match self.raws.get(self.pos) {
            None => Error::Sqlstate("42601", "syntax error at end of jsonpath input".into()),
            Some(raw) => Error::Sqlstate(
                "42601",
                format!("syntax error at or near \"{raw}\" of jsonpath input"),
            ),
        }
    }

    fn predicate(&self, n: Node) -> Result<Node> {
        if n.is_predicate() {
            Ok(n)
        } else {
            Err(self.err())
        }
    }

    fn parse_or(&mut self) -> Result<Node> {
        let mut l = self.parse_and()?;
        while self.eat_punct("||") {
            let r = self.parse_and()?;
            let (l2, r2) = (self.predicate(l)?, self.predicate(r)?);
            l = Node::new(Item::Binary(Op::Or, Box::new(l2), Box::new(r2)));
        }
        Ok(l)
    }

    fn parse_and(&mut self) -> Result<Node> {
        let mut l = self.parse_not()?;
        while self.eat_punct("&&") {
            let r = self.parse_not()?;
            let (l2, r2) = (self.predicate(l)?, self.predicate(r)?);
            l = Node::new(Item::Binary(Op::And, Box::new(l2), Box::new(r2)));
        }
        Ok(l)
    }

    fn parse_not(&mut self) -> Result<Node> {
        if self.eat_punct("!") {
            // `!` takes a delimited predicate: `( predicate )` or exists(...).
            if self.is_kw("exists") {
                let e = self.parse_exists()?;
                return Ok(Node::new(Item::Not(Box::new(e))));
            }
            self.expect_punct("(")?;
            let p = self.parse_or()?;
            let p = self.predicate(p)?;
            self.expect_punct(")")?;
            return Ok(Node::new(Item::Not(Box::new(p))));
        }
        self.parse_cmp()
    }

    fn parse_exists(&mut self) -> Result<Node> {
        self.pos += 1; // exists
        self.expect_punct("(")?;
        let e = self.parse_or()?;
        if e.is_predicate() {
            return Err(self.err());
        }
        self.expect_punct(")")?;
        Ok(Node::new(Item::Exists(Box::new(e))))
    }

    fn parse_cmp(&mut self) -> Result<Node> {
        let l = self.parse_additive()?;
        let op = match self.peek() {
            Some(Tok::Punct("==")) => Some(Op::Eq),
            Some(Tok::Punct("!=" | "<>")) => Some(Op::Ne),
            Some(Tok::Punct("<")) => Some(Op::Lt),
            Some(Tok::Punct(">")) => Some(Op::Gt),
            Some(Tok::Punct("<=")) => Some(Op::Le),
            Some(Tok::Punct(">=")) => Some(Op::Ge),
            _ => None,
        };
        if let Some(op) = op {
            if l.is_predicate() {
                return Err(self.err());
            }
            self.pos += 1;
            let r = self.parse_additive()?;
            if r.is_predicate() {
                return Err(self.err());
            }
            return Ok(Node::new(Item::Binary(op, Box::new(l), Box::new(r))));
        }
        if self.is_kw("starts") && matches!(self.peek_at(1), Some(Tok::Ident(w)) if w == "with") {
            if l.is_predicate() {
                return Err(self.err());
            }
            self.pos += 2;
            let init = match self.peek().cloned() {
                Some(Tok::Str(s)) => Node::new(Item::Str(s)),
                Some(Tok::Var(v)) => Node::new(Item::Var(v)),
                _ => return Err(self.err()),
            };
            self.pos += 1;
            return Ok(Node::new(Item::Binary(
                Op::StartsWith,
                Box::new(l),
                Box::new(init),
            )));
        }
        if self.is_kw("like_regex") {
            if l.is_predicate() {
                return Err(self.err());
            }
            self.pos += 1;
            let Some(Tok::Str(pattern)) = self.peek().cloned() else {
                return Err(self.err());
            };
            self.pos += 1;
            let mut flags = String::new();
            if self.is_kw("flag") {
                self.pos += 1;
                let Some(Tok::Str(f)) = self.peek().cloned() else {
                    return Err(self.err());
                };
                self.pos += 1;
                for ch in f.chars() {
                    if !"ismxq".contains(ch) {
                        return Err(Error::Sqlstate(
                            "42601",
                            format!("invalid input syntax for type jsonpath\nunrecognized flag character \"{ch}\" in LIKE_REGEX predicate"),
                        ));
                    }
                }
                flags = f;
            }
            return Ok(Node::new(Item::LikeRegex(Box::new(l), pattern, flags)));
        }
        Ok(l)
    }

    fn parse_additive(&mut self) -> Result<Node> {
        let mut l = self.parse_mult()?;
        loop {
            let op = if self.is_punct("+") {
                Op::Add
            } else if self.is_punct("-") {
                Op::Sub
            } else {
                return Ok(l);
            };
            if l.is_predicate() {
                return Err(self.err());
            }
            self.pos += 1;
            let r = self.parse_mult()?;
            if r.is_predicate() {
                return Err(self.err());
            }
            l = Node::new(Item::Binary(op, Box::new(l), Box::new(r)));
        }
    }

    fn parse_mult(&mut self) -> Result<Node> {
        let mut l = self.parse_unary()?;
        loop {
            let op = if self.is_punct("*") {
                Op::Mul
            } else if self.is_punct("/") {
                Op::Div
            } else if self.is_punct("%") {
                Op::Mod
            } else {
                return Ok(l);
            };
            if l.is_predicate() {
                return Err(self.err());
            }
            self.pos += 1;
            let r = self.parse_unary()?;
            if r.is_predicate() {
                return Err(self.err());
            }
            l = Node::new(Item::Binary(op, Box::new(l), Box::new(r)));
        }
    }

    fn parse_unary(&mut self) -> Result<Node> {
        if self.eat_punct("+") {
            let e = self.parse_unary()?;
            return Ok(Node::new(Item::Plus(Box::new(e))));
        }
        if self.eat_punct("-") {
            let e = self.parse_unary()?;
            return Ok(Node::new(Item::Minus(Box::new(e))));
        }
        self.parse_accessor_expr()
    }

    fn parse_accessor_expr(&mut self) -> Result<Node> {
        let mut head = match self.peek().cloned() {
            Some(Tok::Punct("(")) => {
                self.pos += 1;
                let inner = self.parse_or()?;
                self.expect_punct(")")?;
                if self.is_kw("is") {
                    self.pos += 1;
                    if !self.is_kw("unknown") {
                        return Err(self.err());
                    }
                    self.pos += 1;
                    let p = self.predicate(inner)?;
                    return Ok(Node::new(Item::IsUnknown(Box::new(p))));
                }
                if !self.at_accessor() {
                    return Ok(inner);
                }
                inner
            }
            Some(Tok::Ident(k))
                if k == "exists" && matches!(self.peek_at(1), Some(Tok::Punct("("))) =>
            {
                return self.parse_exists();
            }
            Some(Tok::Punct("$")) => {
                self.pos += 1;
                Node::new(Item::Root)
            }
            Some(Tok::Punct("@")) => {
                self.pos += 1;
                Node::new(Item::Current)
            }
            Some(Tok::Ident(k)) if k == "last" => {
                self.pos += 1;
                Node::new(Item::Last)
            }
            Some(Tok::Ident(k)) if k == "null" => {
                self.pos += 1;
                Node::new(Item::Null)
            }
            Some(Tok::Ident(k)) if k == "true" || k == "false" => {
                self.pos += 1;
                Node::new(Item::Bool(k == "true"))
            }
            Some(Tok::Str(s)) => {
                self.pos += 1;
                Node::new(Item::Str(s))
            }
            Some(Tok::Var(v)) => {
                self.pos += 1;
                Node::new(Item::Var(v))
            }
            Some(Tok::Num(t)) | Some(Tok::Int(t)) => {
                self.pos += 1;
                let canonical = crate::numeric::canonical_numeric_text(&t)
                    .map_err(|_| syntax(self.input, "invalid numeric literal"))?;
                Node::new(Item::Num(canonical))
            }
            _ => return Err(self.err()),
        };
        while self.at_accessor() {
            let acc = self.parse_accessor()?;
            head.append(acc);
        }
        Ok(head)
    }

    fn at_accessor(&self) -> bool {
        self.is_punct(".") || self.is_punct("[") || self.is_punct("?")
    }

    fn parse_accessor(&mut self) -> Result<Node> {
        if self.eat_punct("?") {
            self.expect_punct("(")?;
            let p = self.parse_or()?;
            let p = self.predicate(p)?;
            self.expect_punct(")")?;
            return Ok(Node::new(Item::Filter(Box::new(p))));
        }
        if self.eat_punct("[") {
            if self.is_punct("*") && matches!(self.peek_at(1), Some(Tok::Punct("]"))) {
                self.pos += 2;
                return Ok(Node::new(Item::AnyArray));
            }
            let mut elems = Vec::new();
            loop {
                let from = self.parse_additive()?;
                let to = if self.is_kw("to") {
                    self.pos += 1;
                    Some(self.parse_additive()?)
                } else {
                    None
                };
                elems.push((from, to));
                if self.eat_punct(",") {
                    continue;
                }
                self.expect_punct("]")?;
                break;
            }
            return Ok(Node::new(Item::Index(elems)));
        }
        self.expect_punct(".")?;
        if self.eat_punct("*") {
            return Ok(Node::new(Item::AnyKey));
        }
        if self.eat_punct("**") {
            let (mut first, mut last) = (0u32, u32::MAX);
            if self.eat_punct("{") {
                let level = |p: &mut Self| -> Result<u32> {
                    match p.peek().cloned() {
                        Some(Tok::Int(t)) => {
                            p.pos += 1;
                            t.parse::<u32>().map_err(|_| p.err())
                        }
                        Some(Tok::Ident(k)) if k == "last" => {
                            p.pos += 1;
                            Ok(u32::MAX)
                        }
                        _ => Err(p.err()),
                    }
                };
                first = level(self)?;
                last = first;
                if self.is_kw("to") {
                    self.pos += 1;
                    last = level(self)?;
                }
                self.expect_punct("}")?;
            }
            return Ok(Node::new(Item::Any(first, last)));
        }
        let key = match self.peek().cloned() {
            Some(Tok::Str(s)) => s,
            Some(Tok::Ident(s)) => s,
            _ => return Err(self.err()),
        };
        self.pos += 1;
        if self.is_punct("(") {
            let method = match key.as_str() {
                "type" => Some(Method::Type),
                "size" => Some(Method::Size),
                "double" => Some(Method::Double),
                "abs" => Some(Method::Abs),
                "floor" => Some(Method::Floor),
                "ceiling" => Some(Method::Ceiling),
                "keyvalue" => Some(Method::KeyValue),
                _ => None,
            };
            if let Some(m) = method {
                self.pos += 1;
                self.expect_punct(")")?;
                return Ok(Node::new(Item::Method(m)));
            }
            if key == "datetime" {
                self.pos += 1;
                let tmpl = match self.peek().cloned() {
                    Some(Tok::Str(s)) => {
                        self.pos += 1;
                        Some(s)
                    }
                    _ => None,
                };
                self.expect_punct(")")?;
                return Ok(Node::new(Item::Datetime(tmpl)));
            }
            return Err(self.err());
        }
        Ok(Node::new(Item::Key(key)))
    }
}

/// Parse a jsonpath's text (`jsonpath_in`).
pub fn parse(text: &str) -> Result<JsonPath> {
    let raw = lex_raw(text)?;
    let (toks, raws): (Vec<Tok>, Vec<String>) = raw.into_iter().unzip();
    let mut p = Parser {
        toks,
        raws,
        pos: 0,
        input: text,
    };
    if p.peek().is_none() {
        return Ok(JsonPath {
            lax: true,
            expr: None,
        });
    }
    let mut lax = true;
    if p.is_kw("strict") {
        lax = false;
        p.pos += 1;
    } else if p.is_kw("lax") {
        p.pos += 1;
    }
    let e = p.parse_or()?;
    if p.peek().is_some() {
        return Err(p.err());
    }
    // `last` only inside an array subscript.
    fn check_last(n: &Node, in_sub: bool) -> Result<()> {
        let check = |x: &Node, s: bool| check_last(x, s);
        match &n.item {
            Item::Last if !in_sub => {
                return Err(Error::Sqlstate(
                    "42601",
                    "LAST is allowed only in array subscripts".into(),
                ))
            }
            Item::Index(elems) => {
                for (a, b) in elems {
                    check(a, true)?;
                    if let Some(b) = b {
                        check(b, true)?;
                    }
                }
            }
            Item::Filter(x)
            | Item::Plus(x)
            | Item::Minus(x)
            | Item::Not(x)
            | Item::IsUnknown(x)
            | Item::Exists(x)
            | Item::LikeRegex(x, _, _) => check(x, in_sub)?,
            Item::Binary(_, a, b) => {
                check(a, in_sub)?;
                check(b, in_sub)?;
            }
            _ => {}
        }
        if let Some(nx) = &n.next {
            check(nx, in_sub)?;
        }
        Ok(())
    }
    check_last(&e, false)?;
    fn check_current(n: &Node, in_filter: bool) -> Result<()> {
        match &n.item {
            Item::Current if !in_filter => {
                return Err(Error::Sqlstate(
                    "42601",
                    "@ is not allowed in root expressions".into(),
                ))
            }
            Item::Filter(x) => check_current(x, true)?,
            Item::Index(elems) => {
                for (a, b) in elems {
                    check_current(a, in_filter)?;
                    if let Some(b) = b {
                        check_current(b, in_filter)?;
                    }
                }
            }
            Item::Plus(x)
            | Item::Minus(x)
            | Item::Not(x)
            | Item::IsUnknown(x)
            | Item::Exists(x)
            | Item::LikeRegex(x, _, _) => check_current(x, in_filter)?,
            Item::Binary(_, a, b) => {
                check_current(a, in_filter)?;
                check_current(b, in_filter)?;
            }
            _ => {}
        }
        if let Some(nx) = &n.next {
            check_current(nx, in_filter)?;
        }
        Ok(())
    }
    check_current(&e, false)?;
    Ok(JsonPath { lax, expr: Some(e) })
}

// ------------------------------------------------------------------------
// Printer (printJsonPathItem)
// ------------------------------------------------------------------------

fn escape_json(s: &str, out: &mut String) {
    out.push_str(&crate::json::render_jsonb(&Json::Str(s.to_string())));
}

fn priority(i: &Item) -> u8 {
    match i {
        Item::Binary(Op::Or, ..) => 0,
        Item::Binary(Op::And, ..) => 1,
        Item::Binary(Op::Eq | Op::Ne | Op::Lt | Op::Gt | Op::Le | Op::Ge | Op::StartsWith, ..) => 2,
        Item::Binary(Op::Add | Op::Sub, ..) => 3,
        Item::Binary(Op::Mul | Op::Div | Op::Mod, ..) => 4,
        Item::Plus(_) | Item::Minus(_) => 5,
        _ => 6,
    }
}

fn print(n: &Node, out: &mut String, in_key: bool, brackets: bool) {
    let sub = |x: &Node, out: &mut String, parent: &Item| {
        print(x, out, false, priority(&x.item) <= priority(parent))
    };
    match &n.item {
        Item::Null => out.push_str("null"),
        Item::Key(k) => {
            if in_key {
                out.push('.');
            }
            escape_json(k, out);
        }
        Item::Str(s) => escape_json(s, out),
        Item::Var(v) => {
            out.push('$');
            escape_json(v, out);
        }
        Item::Num(t) => out.push_str(t),
        Item::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Item::Binary(op, l, r) => {
            if brackets {
                out.push('(');
            }
            sub(l, out, &n.item);
            out.push(' ');
            out.push_str(op.name());
            out.push(' ');
            sub(r, out, &n.item);
            if brackets {
                out.push(')');
            }
        }
        Item::LikeRegex(e, pat, flags) => {
            if brackets {
                out.push('(');
            }
            sub(e, out, &n.item);
            out.push_str(" like_regex ");
            escape_json(pat, out);
            if !flags.is_empty() {
                out.push_str(" flag \"");
                for f in ['i', 's', 'm', 'x', 'q'] {
                    if flags.contains(f) {
                        out.push(f);
                    }
                }
                out.push('"');
            }
            if brackets {
                out.push(')');
            }
        }
        Item::Plus(e) | Item::Minus(e) => {
            if brackets {
                out.push('(');
            }
            out.push(if matches!(n.item, Item::Plus(_)) {
                '+'
            } else {
                '-'
            });
            sub(e, out, &n.item);
            if brackets {
                out.push(')');
            }
        }
        Item::Filter(p) => {
            out.push_str("?(");
            print(p, out, false, false);
            out.push(')');
        }
        Item::Not(p) => {
            out.push_str("!(");
            print(p, out, false, false);
            out.push(')');
        }
        Item::IsUnknown(p) => {
            out.push('(');
            print(p, out, false, false);
            out.push_str(") is unknown");
        }
        Item::Exists(p) => {
            out.push_str("exists (");
            print(p, out, false, false);
            out.push(')');
        }
        Item::Current => out.push('@'),
        Item::Root => out.push('$'),
        Item::Last => out.push_str("last"),
        Item::AnyArray => out.push_str("[*]"),
        Item::AnyKey => {
            if in_key {
                out.push('.');
            }
            out.push('*');
        }
        Item::Index(elems) => {
            out.push('[');
            for (i, (a, b)) in elems.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                print(a, out, false, false);
                if let Some(b) = b {
                    out.push_str(" to ");
                    print(b, out, false, false);
                }
            }
            out.push(']');
        }
        Item::Any(first, last) => {
            if in_key {
                out.push('.');
            }
            let lv = |v: u32| {
                if v == u32::MAX {
                    "last".to_string()
                } else {
                    v.to_string()
                }
            };
            if *first == 0 && *last == u32::MAX {
                out.push_str("**");
            } else if first == last {
                out.push_str(&format!("**{{{}}}", lv(*first)));
            } else {
                out.push_str(&format!("**{{{} to {}}}", lv(*first), lv(*last)));
            }
        }
        Item::Method(m) => out.push_str(&format!(".{}()", m.name())),
        Item::Datetime(t) => {
            out.push_str(".datetime(");
            if let Some(t) = t {
                escape_json(t, out);
            }
            out.push(')');
        }
    }
    if let Some(nx) = &n.next {
        print(nx, out, true, true);
    }
}

/// A jsonpath's canonical text (`jsonpath_out`).
pub fn render(p: &JsonPath) -> String {
    let mut s = String::new();
    if !p.lax {
        s.push_str("strict ");
    }
    if let Some(e) = &p.expr {
        print(e, &mut s, false, true);
    }
    s
}

// ------------------------------------------------------------------------
// Executor
// ------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Res {
    Ok,
    NotFound,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tri {
    True,
    False,
    Unknown,
}

struct Cxt<'a> {
    vars: &'a Json,
    root: &'a Json,
    current: Json,
    lax: bool,
    ignore_structural: bool,
    throw_errors: bool,
    innermost_array_size: i64,
    /// Where the item handed to the next `exec` sits in the root document
    /// (`None`: a computed item, not the document's). `.keyvalue()`'s `id` is
    /// that position's byte offset, which a value alone cannot tell apart
    /// from an equal object elsewhere.
    pending_path: Option<Vec<Step>>,
    /// The position of `@` inside a filter.
    current_path: Option<Vec<Step>>,
}

/// One step from a container to a child.
#[derive(Debug, Clone, PartialEq)]
enum Step {
    Key(String),
    Idx(usize),
}

fn child_path(base: &Option<Vec<Step>>, step: Step) -> Option<Vec<Step>> {
    base.as_ref().map(|b| {
        let mut p = b.clone();
        p.push(step);
        p
    })
}

fn sqlerr(code: &'static str, msg: impl Into<String>) -> Error {
    Error::Sqlstate(code, msg.into())
}

/// `RETURN_ERROR`: raise when errors are thrown, else report `jperError`.
macro_rules! ret_err {
    ($cxt:expr, $e:expr) => {
        if $cxt.throw_errors {
            return Err($e);
        } else {
            return Ok(Res::Error);
        }
    };
}

fn jtype(v: &Json) -> &'static str {
    if let Some(d) = dt_of(v) {
        return match d.kind.as_str() {
            "date" => "date",
            "time" => "time without time zone",
            "timetz" => "time with time zone",
            "timestamp" => "timestamp without time zone",
            _ => "timestamp with time zone",
        };
    }
    match v {
        Json::Null => "null",
        Json::Bool(_) => "boolean",
        Json::Number(_) => "number",
        Json::Str(_) => "string",
        Json::Array(_) => "array",
        Json::Object(_) => "object",
    }
}

fn num_text(v: &Json) -> Option<&str> {
    match v {
        Json::Number(t) => Some(t.as_str()),
        _ => None,
    }
}

fn canon(t: &str) -> String {
    crate::numeric::canonical_numeric_text(t).unwrap_or_else(|_| t.to_string())
}

impl Cxt<'_> {
    fn auto_unwrap(&self) -> bool {
        self.lax
    }

    fn auto_wrap(&self) -> bool {
        self.lax
    }

    fn strict_absence_of_errors(&self) -> bool {
        !self.lax
    }

    fn exec(&mut self, n: &Node, jb: &Json, found: Option<&mut Vec<Json>>) -> Result<Res> {
        let unwrap = self.auto_unwrap();
        self.exec_opt(n, jb, found, unwrap)
    }

    /// `executeNextItem`: continue with `n.next`, or collect `v`.
    fn next(&mut self, n: &Node, v: Json, found: Option<&mut Vec<Json>>) -> Result<Res> {
        match &n.next {
            Some(nx) => self.exec(nx, &v, found),
            None => {
                if let Some(f) = found {
                    f.push(v);
                }
                Ok(Res::Ok)
            }
        }
    }

    fn has_next(n: &Node) -> bool {
        n.next.is_some()
    }

    /// `executeItemUnwrapTargetArray` / `executeAnyItem` over one level.
    fn unwrap_array(
        &mut self,
        n: Option<&Node>,
        items: &[Json],
        mut found: Option<&mut Vec<Json>>,
        unwrap_elems: bool,
        base: Option<Vec<Step>>,
    ) -> Result<Res> {
        let mut res = Res::NotFound;
        for (i, v) in items.iter().enumerate() {
            match n {
                Some(n) => {
                    self.pending_path = child_path(&base, Step::Idx(i));
                    res = self.exec_opt(n, v, found.as_deref_mut(), unwrap_elems)?;
                    if res == Res::Error {
                        break;
                    }
                    if res == Res::Ok && found.is_none() {
                        break;
                    }
                }
                None => match found.as_deref_mut() {
                    Some(f) => f.push(v.clone()),
                    None => return Ok(Res::Ok),
                },
            }
        }
        Ok(res)
    }

    /// `executeAnyItem` for `.*`, `[*]` in lax mode's element unwrapping, and
    /// `.**`: the children of a container, recursing to `last`.
    #[allow(clippy::too_many_arguments)]
    fn any_item(
        &mut self,
        n: Option<&Node>,
        container: &Json,
        mut found: Option<&mut Vec<Json>>,
        level: u32,
        first: u32,
        last: u32,
        ignore_structural: bool,
        unwrap_next: bool,
        base: Option<Vec<Step>>,
    ) -> Result<Res> {
        let mut res = Res::NotFound;
        if level > last {
            return Ok(res);
        }
        let children: Vec<(Step, &Json)> = match container {
            Json::Array(a) => a
                .iter()
                .enumerate()
                .map(|(i, v)| (Step::Idx(i), v))
                .collect(),
            Json::Object(o) => o.iter().map(|(k, v)| (Step::Key(k.clone()), v)).collect(),
            _ => Vec::new(),
        };
        for (step, v) in children {
            let here = child_path(&base, step);
            if level >= first
                || (first == u32::MAX
                    && last == u32::MAX
                    && !matches!(v, Json::Array(_) | Json::Object(_)))
            {
                match n {
                    Some(n) => {
                        let saved = self.ignore_structural;
                        if ignore_structural {
                            self.ignore_structural = true;
                        }
                        self.pending_path = here.clone();
                        let r = self.exec_opt(n, v, found.as_deref_mut(), unwrap_next);
                        self.ignore_structural = saved;
                        res = r?;
                        if res == Res::Error {
                            break;
                        }
                        if res == Res::Ok && found.is_none() {
                            break;
                        }
                    }
                    None => match found.as_deref_mut() {
                        Some(f) => f.push(v.clone()),
                        None => return Ok(Res::Ok),
                    },
                }
            }
            if level < last && matches!(v, Json::Array(_) | Json::Object(_)) {
                res = self.any_item(
                    n,
                    v,
                    found.as_deref_mut(),
                    level + 1,
                    first,
                    last,
                    ignore_structural,
                    unwrap_next,
                    here.clone(),
                )?;
                if res == Res::Error {
                    break;
                }
                if res == Res::Ok && found.is_none() {
                    break;
                }
            }
        }
        Ok(res)
    }

    fn exec_opt(
        &mut self,
        n: &Node,
        jb: &Json,
        mut found: Option<&mut Vec<Json>>,
        unwrap: bool,
    ) -> Result<Res> {
        let here = self.pending_path.take();
        match &n.item {
            Item::Binary(
                Op::And
                | Op::Or
                | Op::Eq
                | Op::Ne
                | Op::Lt
                | Op::Gt
                | Op::Le
                | Op::Ge
                | Op::StartsWith,
                ..,
            )
            | Item::Not(_)
            | Item::IsUnknown(_)
            | Item::Exists(_)
            | Item::LikeRegex(..) => {
                let st = self.bool_item(n, jb)?;
                if !Self::has_next(n) && found.is_none() {
                    return Ok(Res::Ok);
                }
                let v = match st {
                    Tri::Unknown => Json::Null,
                    Tri::True => Json::Bool(true),
                    Tri::False => Json::Bool(false),
                };
                self.next(n, v, found)
            }
            Item::Key(k) => match jb {
                Json::Object(o) => match o.iter().rev().find(|(kk, _)| kk == k) {
                    Some((_, v)) => {
                        let v = v.clone();
                        self.pending_path = child_path(&here, Step::Key(k.clone()));
                        self.next(n, v, found)
                    }
                    None => {
                        if !self.ignore_structural {
                            ret_err!(
                                self,
                                sqlerr(
                                    "2203A",
                                    format!("JSON object does not contain key \"{k}\"")
                                )
                            );
                        }
                        Ok(Res::NotFound)
                    }
                },
                Json::Array(a) if unwrap => {
                    let a = a.clone();
                    self.unwrap_array(Some(n), &a, found, false, here.clone())
                }
                _ => {
                    if !self.ignore_structural {
                        ret_err!(
                            self,
                            sqlerr(
                                "2203A",
                                "jsonpath member accessor can only be applied to an object"
                            )
                        );
                    }
                    Ok(Res::NotFound)
                }
            },
            Item::Root => {
                let root = self.root.clone();
                self.pending_path = Some(Vec::new());
                self.next(n, root, found)
            }
            Item::Current => {
                let cur = self.current.clone();
                self.pending_path = self.current_path.clone();
                self.next(n, cur, found)
            }
            Item::AnyArray => {
                match jb {
                    Json::Array(a) => {
                        let a = a.clone();
                        let unwrap_elems = self.auto_unwrap();
                        self.unwrap_array(n.next.as_deref(), &a, found, unwrap_elems, here)
                    }
                    _ if self.auto_wrap() => {
                        self.pending_path = here;
                        self.next(n, jb.clone(), found)
                    }
                    _ => {
                        if !self.ignore_structural {
                            ret_err!(self, sqlerr("22039", "jsonpath wildcard array accessor can only be applied to an array"));
                        }
                        Ok(Res::NotFound)
                    }
                }
            }
            Item::Index(elems) => {
                let arr: Option<Vec<Json>> = match jb {
                    Json::Array(a) => Some(a.clone()),
                    _ if self.auto_wrap() => None,
                    _ => {
                        if !self.ignore_structural {
                            ret_err!(
                                self,
                                sqlerr(
                                    "22039",
                                    "jsonpath array accessor can only be applied to an array"
                                )
                            );
                        }
                        return Ok(Res::NotFound);
                    }
                };
                let singleton = arr.is_none();
                let size = arr.as_ref().map_or(1, |a| a.len() as i64);
                let saved = self.innermost_array_size;
                self.innermost_array_size = size;
                let mut res = Res::NotFound;
                for (from, to) in elems {
                    let (r, ifrom) = self.array_index(from, jb)?;
                    if r == Res::Error {
                        res = r;
                        break;
                    }
                    let ito = match to {
                        Some(to) => {
                            let (r2, v) = self.array_index(to, jb)?;
                            if r2 == Res::Error {
                                res = r2;
                                break;
                            }
                            v
                        }
                        None => ifrom,
                    };
                    if !self.ignore_structural && (ifrom < 0 || ifrom > ito || ito >= size) {
                        self.innermost_array_size = saved;
                        ret_err!(
                            self,
                            sqlerr("22033", "jsonpath array subscript is out of bounds")
                        );
                    }
                    let (lo, hi) = (ifrom.max(0), ito.min(size - 1));
                    res = Res::NotFound;
                    let mut stop = false;
                    let mut idx = lo;
                    while idx <= hi {
                        let v = if singleton {
                            self.pending_path = here.clone();
                            jb.clone()
                        } else {
                            self.pending_path = child_path(&here, Step::Idx(idx as usize));
                            arr.as_ref().expect("array")[idx as usize].clone()
                        };
                        if !Self::has_next(n) && found.is_none() {
                            self.innermost_array_size = saved;
                            return Ok(Res::Ok);
                        }
                        res = self.next(n, v, found.as_deref_mut())?;
                        if res == Res::Error || (res == Res::Ok && found.is_none()) {
                            stop = true;
                            break;
                        }
                        idx += 1;
                    }
                    if stop {
                        break;
                    }
                }
                self.innermost_array_size = saved;
                Ok(res)
            }
            Item::Last => {
                if self.innermost_array_size < 0 {
                    return Err(sqlerr(
                        "XX000",
                        "evaluating jsonpath LAST outside of array subscript",
                    ));
                }
                if !Self::has_next(n) && found.is_none() {
                    return Ok(Res::Ok);
                }
                let v = Json::Number((self.innermost_array_size - 1).to_string());
                self.next(n, v, found)
            }
            Item::AnyKey => match jb {
                Json::Object(_) => {
                    let c = jb.clone();
                    let unwrap_next = self.auto_unwrap();
                    self.any_item(
                        n.next.as_deref(),
                        &c,
                        found,
                        1,
                        1,
                        1,
                        false,
                        unwrap_next,
                        here,
                    )
                }
                Json::Array(a) if unwrap => {
                    let a = a.clone();
                    self.unwrap_array(Some(n), &a, found, false, here.clone())
                }
                _ => {
                    if !self.ignore_structural {
                        ret_err!(self, sqlerr("2203C", "jsonpath wildcard member accessor can only be applied to an object"));
                    }
                    Ok(Res::NotFound)
                }
            },
            Item::Binary(op @ (Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Mod), l, r) => {
                self.binary_arith(n, *op, l, r, jb, found)
            }
            Item::Plus(e) => self.unary_arith(n, e, jb, found, false),
            Item::Minus(e) => self.unary_arith(n, e, jb, found, true),
            Item::Filter(p) => {
                if unwrap {
                    if let Json::Array(a) = jb {
                        let a = a.clone();
                        return self.unwrap_array(Some(n), &a, found, false, here.clone());
                    }
                }
                let prev = std::mem::replace(&mut self.current, jb.clone());
                let prev_path = std::mem::replace(&mut self.current_path, here.clone());
                let st = self.bool_item(p, jb);
                self.current = prev;
                self.current_path = prev_path;
                if st? != Tri::True {
                    Ok(Res::NotFound)
                } else {
                    self.pending_path = here;
                    self.next(n, jb.clone(), found)
                }
            }
            Item::Any(first, last) => {
                let mut res = Res::NotFound;
                if *first == 0 {
                    let saved = self.ignore_structural;
                    self.ignore_structural = true;
                    let r = match &n.next {
                        Some(nx) => {
                            self.pending_path = here.clone();
                            self.exec(nx, jb, found.as_deref_mut())
                        }
                        None => {
                            if let Some(f) = found.as_deref_mut() {
                                f.push(jb.clone());
                            }
                            Ok(Res::Ok)
                        }
                    };
                    self.ignore_structural = saved;
                    res = r?;
                    if res == Res::Ok && found.is_none() {
                        return Ok(res);
                    }
                }
                if matches!(jb, Json::Array(_) | Json::Object(_)) {
                    let unwrap_next = self.auto_unwrap();
                    res = self.any_item(
                        n.next.as_deref(),
                        jb,
                        found,
                        1,
                        *first,
                        *last,
                        true,
                        unwrap_next,
                        here.clone(),
                    )?;
                }
                Ok(res)
            }
            Item::Null | Item::Bool(_) | Item::Num(_) | Item::Str(_) | Item::Var(_) => {
                if !Self::has_next(n) && found.is_none() && !matches!(n.item, Item::Var(_)) {
                    return Ok(Res::Ok);
                }
                let v = match &n.item {
                    Item::Null => Json::Null,
                    Item::Bool(b) => Json::Bool(*b),
                    Item::Num(t) => Json::Number(t.clone()),
                    Item::Str(s) => Json::Str(s.clone()),
                    Item::Var(name) => self.variable(name)?,
                    _ => unreachable!(),
                };
                self.next(n, v, found)
            }
            Item::Method(Method::Type) => {
                let v = Json::Str(jtype(jb).to_string());
                self.next(n, v, found)
            }
            Item::Method(Method::Size) => {
                let size = match jb {
                    Json::Array(a) => a.len() as i64,
                    _ => {
                        if !self.auto_wrap() {
                            if !self.ignore_structural {
                                ret_err!(self, sqlerr("22039", "jsonpath item method .size() can only be applied to an array"));
                            }
                            return Ok(Res::NotFound);
                        }
                        1
                    }
                };
                self.next(n, Json::Number(size.to_string()), found)
            }
            Item::Method(m @ (Method::Abs | Method::Floor | Method::Ceiling)) => {
                if unwrap {
                    if let Json::Array(a) = jb {
                        let a = a.clone();
                        return self.unwrap_array(Some(n), &a, found, false, here.clone());
                    }
                }
                let Some(t) = num_text(jb) else {
                    ret_err!(
                        self,
                        sqlerr(
                            "22036",
                            format!(
                                "jsonpath item method .{}() can only be applied to a numeric value",
                                m.name()
                            )
                        )
                    );
                };
                let op = match m {
                    Method::Abs => "abs",
                    Method::Floor => "floor",
                    _ => "ceil",
                };
                let out = crate::numeric::decimal_unary_or_mod(op, t, None)
                    .unwrap_or_else(|| Ok(t.to_string()))?;
                if !Self::has_next(n) && found.is_none() {
                    return Ok(Res::Ok);
                }
                self.next(n, Json::Number(out), found)
            }
            Item::Method(Method::Double) => {
                if unwrap {
                    if let Json::Array(a) = jb {
                        let a = a.clone();
                        return self.unwrap_array(Some(n), &a, found, false, here.clone());
                    }
                }
                let v = match jb {
                    Json::Number(t) => {
                        let f = t.parse::<f64>().ok().filter(|f| f.is_finite());
                        if f.is_none() {
                            ret_err!(self, sqlerr("22036", "numeric argument of jsonpath item method .double() is out of range for type double precision"));
                        }
                        jb.clone()
                    }
                    Json::Str(s) => {
                        let f = s
                            .trim()
                            .parse::<f64>()
                            .ok()
                            .filter(|f| f.is_finite() && !s.trim().is_empty());
                        match f {
                            Some(f) => Json::Number(canon(&float8_text(f))),
                            None => {
                                ret_err!(self, sqlerr("22036", "string argument of jsonpath item method .double() is not a valid representation of a double precision number"));
                            }
                        }
                    }
                    _ => {
                        ret_err!(self, sqlerr("22036", "jsonpath item method .double() can only be applied to a string or numeric value"));
                    }
                };
                self.next(n, v, found)
            }
            Item::Method(Method::KeyValue) => {
                if unwrap {
                    if let Json::Array(a) = jb {
                        let a = a.clone();
                        return self.unwrap_array(Some(n), &a, found, false, here.clone());
                    }
                }
                let Json::Object(o) = jb else {
                    ret_err!(
                        self,
                        sqlerr(
                            "2203C",
                            "jsonpath item method .keyvalue() can only be applied to an object"
                        )
                    );
                };
                if o.is_empty() {
                    return Ok(Res::NotFound);
                }
                // PostgreSQL's id is the object's byte offset inside the
                // binary jsonb of the document it came from: found by the
                // object's POSITION, since equal objects sit at different
                // offsets.
                let id = here
                    .as_ref()
                    .and_then(|path| offset_at(self.root, path))
                    .or_else(|| container_offset(self.root, jb))
                    .unwrap_or(0);
                let mut res = Res::NotFound;
                let pairs = normalized_pairs(o);
                for (k, v) in pairs {
                    res = Res::Ok;
                    if !Self::has_next(n) && found.is_none() {
                        break;
                    }
                    let obj = Json::Object(vec![
                        ("id".to_string(), Json::Number(id.to_string())),
                        ("key".to_string(), Json::Str(k)),
                        ("value".to_string(), v),
                    ]);
                    res = self.next(n, obj, found.as_deref_mut())?;
                    if res == Res::Error {
                        return Ok(res);
                    }
                    if res == Res::Ok && found.is_none() {
                        break;
                    }
                }
                Ok(res)
            }
            Item::Datetime(tmpl) => {
                if unwrap {
                    if let Json::Array(a) = jb {
                        let a = a.clone();
                        return self.unwrap_array(Some(n), &a, found, false, here.clone());
                    }
                }
                let Json::Str(text) = jb else {
                    ret_err!(
                        self,
                        sqlerr(
                            "22031",
                            "jsonpath item method .datetime() can only be applied to a string"
                        )
                    );
                };
                let item = match parse_datetime(text, tmpl.as_deref()) {
                    Ok(item) => item,
                    Err(e) => ret_err!(self, e),
                };
                self.next(n, item, found)
            }
        }
    }

    fn variable(&self, name: &str) -> Result<Json> {
        if let Json::Object(o) = self.vars {
            if let Some((_, v)) = o.iter().rev().find(|(k, _)| k == name) {
                return Ok(v.clone());
            }
        }
        Err(sqlerr(
            "42704",
            format!("could not find jsonpath variable \"{name}\""),
        ))
    }

    fn array_index(&mut self, n: &Node, jb: &Json) -> Result<(Res, i64)> {
        let mut found = Vec::new();
        let r = self.exec(n, jb, Some(&mut found))?;
        if r == Res::Error {
            return Ok((r, 0));
        }
        let t = match found.as_slice() {
            [Json::Number(t)] => t.clone(),
            _ => {
                if self.throw_errors {
                    return Err(sqlerr(
                        "22033",
                        "jsonpath array subscript is not a single numeric value",
                    ));
                }
                return Ok((Res::Error, 0));
            }
        };
        let trunc = t.split('.').next().unwrap_or("0");
        match trunc.parse::<i32>() {
            Ok(v) => Ok((Res::Ok, i64::from(v))),
            Err(_) => {
                if self.throw_errors {
                    return Err(sqlerr(
                        "22033",
                        "jsonpath array subscript is out of integer range",
                    ));
                }
                Ok((Res::Error, 0))
            }
        }
    }

    /// `executeItemOptUnwrapResult`: the results, arrays unwrapped in lax.
    fn opt_unwrap_result(
        &mut self,
        n: &Node,
        jb: &Json,
        unwrap: bool,
        found: &mut Vec<Json>,
    ) -> Result<Res> {
        if unwrap && self.auto_unwrap() {
            let mut seq = Vec::new();
            let r = self.exec(n, jb, Some(&mut seq))?;
            if r == Res::Error {
                return Ok(r);
            }
            for item in seq {
                match item {
                    Json::Array(a) => found.extend(a),
                    other => found.push(other),
                }
            }
            return Ok(Res::Ok);
        }
        self.exec(n, jb, Some(found))
    }

    /// The `...NoThrow` variant: errors become `jperError`.
    fn opt_unwrap_result_no_throw(
        &mut self,
        n: &Node,
        jb: &Json,
        unwrap: bool,
        found: &mut Vec<Json>,
    ) -> Result<Res> {
        let saved = self.throw_errors;
        self.throw_errors = false;
        let r = self.opt_unwrap_result(n, jb, unwrap, found);
        self.throw_errors = saved;
        match r {
            // A variable that does not exist still raises.
            Err(e @ Error::Sqlstate("42704", _)) => Err(e),
            Err(_) => Ok(Res::Error),
            ok => ok,
        }
    }

    fn binary_arith(
        &mut self,
        n: &Node,
        op: Op,
        l: &Node,
        r: &Node,
        jb: &Json,
        found: Option<&mut Vec<Json>>,
    ) -> Result<Res> {
        let mut lseq = Vec::new();
        if self.opt_unwrap_result(l, jb, true, &mut lseq)? == Res::Error {
            return Ok(Res::Error);
        }
        let mut rseq = Vec::new();
        if self.opt_unwrap_result(r, jb, true, &mut rseq)? == Res::Error {
            return Ok(Res::Error);
        }
        let lt = match lseq.as_slice() {
            [Json::Number(t)] => t.clone(),
            _ => {
                ret_err!(
                    self,
                    sqlerr(
                        "22038",
                        format!(
                            "left operand of jsonpath operator {} is not a single numeric value",
                            op.name()
                        )
                    )
                );
            }
        };
        let rt = match rseq.as_slice() {
            [Json::Number(t)] => t.clone(),
            _ => {
                ret_err!(
                    self,
                    sqlerr(
                        "22038",
                        format!(
                            "right operand of jsonpath operator {} is not a single numeric value",
                            op.name()
                        )
                    )
                );
            }
        };
        let out: Result<String> = match op {
            Op::Mod => crate::numeric::decimal_unary_or_mod("%", &lt, Some(&rt))
                .unwrap_or_else(|| Ok("NaN".into())),
            _ => {
                let sym = match op {
                    Op::Add => "+",
                    Op::Sub => "-",
                    Op::Mul => "*",
                    _ => "/",
                };
                match crate::numeric::decimal_arith(sym, &lt, &rt) {
                    Some(Ok(b)) => Ok(crate::numeric::numeric_text(&b).unwrap_or_default()),
                    Some(Err(e)) => Err(e),
                    None => Err(sqlerr("22003", "numeric field overflow")),
                }
            }
        };
        let value = match out {
            Ok(v) => v,
            Err(e) => {
                if self.throw_errors {
                    return Err(e);
                }
                return Ok(Res::Error);
            }
        };
        if !Self::has_next(n) && found.is_none() {
            return Ok(Res::Ok);
        }
        self.next(n, Json::Number(value), found)
    }

    fn unary_arith(
        &mut self,
        n: &Node,
        e: &Node,
        jb: &Json,
        mut found: Option<&mut Vec<Json>>,
        minus: bool,
    ) -> Result<Res> {
        let mut seq = Vec::new();
        if self.opt_unwrap_result(e, jb, true, &mut seq)? == Res::Error {
            return Ok(Res::Error);
        }
        let mut res = Res::NotFound;
        let has_next = Self::has_next(n);
        for v in seq {
            let Json::Number(t) = &v else {
                if found.is_none() && !has_next {
                    continue;
                }
                let name = if minus { "-" } else { "+" };
                ret_err!(
                    self,
                    sqlerr(
                        "2203B",
                        format!("operand of unary jsonpath operator {name} is not a numeric value")
                    )
                );
            };
            if found.is_none() && !has_next {
                return Ok(Res::Ok);
            }
            let out = if minus {
                crate::numeric::negate_numeric_text(t)
                    .ok()
                    .and_then(|b| crate::numeric::numeric_text(&b))
                    .unwrap_or_else(|| t.clone())
            } else {
                t.clone()
            };
            let r = self.next(n, Json::Number(out), found.as_deref_mut())?;
            if r == Res::Error {
                return Ok(r);
            }
            if r == Res::Ok {
                if found.is_none() {
                    return Ok(Res::Ok);
                }
                res = Res::Ok;
            }
        }
        Ok(res)
    }

    fn bool_item(&mut self, n: &Node, jb: &Json) -> Result<Tri> {
        Ok(match &n.item {
            Item::Binary(Op::And, l, r) => {
                let a = self.bool_item(l, jb)?;
                if a == Tri::False {
                    return Ok(Tri::False);
                }
                let b = self.bool_item(r, jb)?;
                if b == Tri::True {
                    a
                } else {
                    b
                }
            }
            Item::Binary(Op::Or, l, r) => {
                let a = self.bool_item(l, jb)?;
                if a == Tri::True {
                    return Ok(Tri::True);
                }
                let b = self.bool_item(r, jb)?;
                if b == Tri::False {
                    a
                } else {
                    b
                }
            }
            Item::Not(e) => match self.bool_item(e, jb)? {
                Tri::Unknown => Tri::Unknown,
                Tri::True => Tri::False,
                Tri::False => Tri::True,
            },
            Item::IsUnknown(e) => {
                if self.bool_item(e, jb)? == Tri::Unknown {
                    Tri::True
                } else {
                    Tri::False
                }
            }
            Item::Binary(op @ (Op::Eq | Op::Ne | Op::Lt | Op::Gt | Op::Le | Op::Ge), l, r) => {
                let op = *op;
                self.predicate(l, Some(r), jb, true, &mut |a, b| {
                    compare(op, a, b.expect("right"))
                })?
            }
            Item::Binary(Op::StartsWith, l, r) => self.predicate(
                l,
                Some(r),
                jb,
                false,
                &mut |a, b| match (a, b.expect("right")) {
                    (Json::Str(w), Json::Str(i)) => {
                        if w.starts_with(i.as_str()) {
                            Tri::True
                        } else {
                            Tri::False
                        }
                    }
                    _ => Tri::Unknown,
                },
            )?,
            Item::LikeRegex(e, pattern, flags) => {
                let re = build_regex(pattern, flags)?;
                self.predicate(e, None, jb, false, &mut |a, _| match a {
                    Json::Str(s) => {
                        if re.is_match(s) {
                            Tri::True
                        } else {
                            Tri::False
                        }
                    }
                    _ => Tri::Unknown,
                })?
            }
            Item::Exists(e) => {
                let mut vals = Vec::new();
                let r = self.opt_unwrap_result_no_throw(e, jb, false, &mut vals)?;
                if r == Res::Error {
                    Tri::Unknown
                } else if self.strict_absence_of_errors() {
                    if vals.is_empty() {
                        Tri::False
                    } else {
                        Tri::True
                    }
                } else if vals.is_empty() {
                    Tri::False
                } else {
                    Tri::True
                }
            }
            _ => return Err(sqlerr("XX000", "invalid boolean jsonpath item")),
        })
    }

    fn predicate(
        &mut self,
        l: &Node,
        r: Option<&Node>,
        jb: &Json,
        unwrap_right: bool,
        exec: &mut dyn FnMut(&Json, Option<&Json>) -> Tri,
    ) -> Result<Tri> {
        let mut lseq = Vec::new();
        if self.opt_unwrap_result_no_throw(l, jb, true, &mut lseq)? == Res::Error {
            return Ok(Tri::Unknown);
        }
        let mut rseq = Vec::new();
        if let Some(r) = r {
            if self.opt_unwrap_result_no_throw(r, jb, unwrap_right, &mut rseq)? == Res::Error {
                return Ok(Tri::Unknown);
            }
        }
        let (mut error, mut found) = (false, false);
        for lv in &lseq {
            let rights: Vec<Option<&Json>> = if r.is_some() {
                rseq.iter().map(Some).collect()
            } else {
                vec![None]
            };
            for rv in rights {
                match exec(lv, rv) {
                    Tri::Unknown => {
                        if self.strict_absence_of_errors() {
                            return Ok(Tri::Unknown);
                        }
                        error = true;
                    }
                    Tri::True => {
                        if !self.strict_absence_of_errors() {
                            return Ok(Tri::True);
                        }
                        found = true;
                    }
                    Tri::False => {}
                }
            }
        }
        Ok(if found {
            Tri::True
        } else if error {
            Tri::Unknown
        } else {
            Tri::False
        })
    }
}

/// The size of a numeric's varlena as jsonb stores it: `numeric_in`'s
/// result, short-header when it can be (4-byte length, a 2-byte header and
/// 2 bytes per base-10000 digit group, leading and trailing zero groups
/// stripped).
fn numeric_size(text: &str) -> usize {
    let t = canon(text);
    let t = t.trim_start_matches('-');
    let (int, frac) = t.split_once('.').unwrap_or((t, ""));
    let dscale = frac.len();
    let int = int.trim_start_matches('0');
    let lead = (4 - int.len() % 4) % 4;
    let int_padded = format!("{}{int}", "0".repeat(lead));
    let frac_padded = format!("{frac}{}", "0".repeat((4 - frac.len() % 4) % 4));
    let mut groups: Vec<&str> = Vec::new();
    for k in (0..int_padded.len()).step_by(4) {
        groups.push(&int_padded[k..k + 4]);
    }
    let int_groups = groups.len() as i64;
    for k in (0..frac_padded.len()).step_by(4) {
        groups.push(&frac_padded[k..k + 4]);
    }
    let first = groups.iter().position(|g| *g != "0000");
    let Some(first) = first else { return 4 + 2 };
    let last = groups.iter().rposition(|g| *g != "0000").unwrap_or(first);
    let ndigits = last - first + 1;
    let weight = int_groups - 1 - first as i64;
    let short = dscale <= 63 && (-64..=63).contains(&weight);
    4 + if short { 2 } else { 4 } + 2 * ndigits
}

fn align4(p: usize) -> usize {
    (p + 3) & !3
}

/// Lay `v` out as `convertToJsonb` does from position `p`, recording each
/// container's start; the position after it.
fn layout<'a>(v: &'a Json, p: usize, out: &mut Vec<(usize, &'a Json)>) -> usize {
    match v {
        Json::Object(o) => {
            let start = align4(p);
            out.push((start, v));
            let pairs = normalized_pairs(o);
            let mut q = start + 4 + 8 * pairs.len();
            for (k, _) in &pairs {
                q += k.len();
            }
            // Values in the same order, laid out from the original entries.
            for (k, _) in &pairs {
                let val = o
                    .iter()
                    .rev()
                    .find(|(kk, _)| kk == k)
                    .map(|(_, v)| v)
                    .expect("key");
                q = layout(val, q, out);
            }
            q
        }
        Json::Array(a) => {
            let start = align4(p);
            out.push((start, v));
            let mut q = start + 4 + 4 * a.len();
            for e in a {
                q = layout(e, q, out);
            }
            q
        }
        Json::Str(s) => p + s.len(),
        Json::Number(t) => align4(p) + numeric_size(t),
        Json::Bool(_) | Json::Null => p,
    }
}

/// The byte offset of the container at `path` within `root`'s binary jsonb.
fn offset_at(root: &Json, path: &[Step]) -> Option<usize> {
    fn walk(v: &Json, p: usize, path: &[Step]) -> Option<usize> {
        let Some((step, rest)) = path.split_first() else {
            return matches!(v, Json::Object(_) | Json::Array(_)).then(|| align4(p));
        };
        match (v, step) {
            (Json::Object(o), Step::Key(key)) => {
                let start = align4(p);
                let pairs = normalized_pairs(o);
                let mut q = start + 4 + 8 * pairs.len();
                for (k, _) in &pairs {
                    q += k.len();
                }
                for (k, _) in &pairs {
                    let val = o.iter().rev().find(|(kk, _)| kk == k).map(|(_, v)| v)?;
                    if k == key {
                        return walk(val, q, rest);
                    }
                    q = layout(val, q, &mut Vec::new());
                }
                None
            }
            (Json::Array(a), Step::Idx(i)) => {
                let start = align4(p);
                let mut q = start + 4 + 4 * a.len();
                for (j, e) in a.iter().enumerate() {
                    if j == *i {
                        return walk(e, q, rest);
                    }
                    q = layout(e, q, &mut Vec::new());
                }
                None
            }
            _ => None,
        }
    }
    walk(root, 0, path)
}

/// The byte offset of `target` (the first container equal to it) within
/// `root`'s binary jsonb.
fn container_offset(root: &Json, target: &Json) -> Option<usize> {
    let mut out = Vec::new();
    layout(root, 0, &mut out);
    out.into_iter().find(|(_, v)| *v == target).map(|(p, _)| p)
}

/// An object's pairs in jsonb order (by key length, then bytes), last
/// duplicate kept.
fn normalized_pairs(o: &[(String, Json)]) -> Vec<(String, Json)> {
    let mut v: Vec<(String, Json)> = Vec::new();
    for (k, val) in o {
        if let Some(p) = v.iter_mut().find(|(kk, _)| kk == k) {
            p.1 = val.clone();
        } else {
            v.push((k.clone(), val.clone()));
        }
    }
    v.sort_by(|a, b| {
        a.0.len()
            .cmp(&b.0.len())
            .then_with(|| a.0.as_bytes().cmp(b.0.as_bytes()))
    });
    v
}

fn float8_text(f: f64) -> String {
    // float8out's shortest round-trip digits.
    let s = format!("{f}");
    if s.contains('e') {
        format!("{f:e}")
    } else {
        s
    }
}

/// `compareItems`.
fn compare(op: Op, a: &Json, b: &Json) -> Tri {
    // Two datetime items compare as their kinds allow; a datetime against
    // anything else is UNKNOWN (or the null rule below).
    if let (Some(x), Some(y)) = (dt_of(a), dt_of(b)) {
        return match compare_datetimes(&x, &y) {
            Ok(Some(ord)) => tri_of(op, ord),
            Ok(None) => Tri::Unknown,
            Err(e) => {
                DT_ERROR.with(|c| *c.borrow_mut() = Some(e));
                Tri::Unknown
            }
        };
    }
    let same_kind = std::mem::discriminant(a) == std::mem::discriminant(b);
    if !same_kind {
        if matches!(a, Json::Null) || matches!(b, Json::Null) {
            return if op == Op::Ne { Tri::True } else { Tri::False };
        }
        return Tri::Unknown;
    }
    let cmp = match (a, b) {
        (Json::Null, Json::Null) => std::cmp::Ordering::Equal,
        (Json::Bool(x), Json::Bool(y)) => x.cmp(y),
        (Json::Number(x), Json::Number(y)) => {
            crate::numeric::compare_decimal_text(&canon(x), &canon(y))
                .unwrap_or(std::cmp::Ordering::Equal)
        }
        (Json::Str(x), Json::Str(y)) => {
            if op == Op::Eq {
                return if x == y { Tri::True } else { Tri::False };
            }
            x.as_bytes().cmp(y.as_bytes())
        }
        _ => return Tri::Unknown,
    };
    let r = match op {
        Op::Eq => cmp.is_eq(),
        Op::Ne => cmp.is_ne(),
        Op::Lt => cmp.is_lt(),
        Op::Gt => cmp.is_gt(),
        Op::Le => cmp.is_le(),
        Op::Ge => cmp.is_ge(),
        _ => false,
    };
    if r {
        Tri::True
    } else {
        Tri::False
    }
}

fn build_regex(pattern: &str, flags: &str) -> Result<regex::Regex> {
    let mut prefix = String::new();
    for f in flags.chars() {
        match f {
            'i' => prefix.push('i'),
            's' => prefix.push('s'),
            'm' => prefix.push('m'),
            'x' => prefix.push('x'),
            _ => {}
        }
    }
    let body = if flags.contains('q') {
        regex::escape(pattern)
    } else {
        pattern.to_string()
    };
    let full = if prefix.is_empty() {
        body
    } else {
        format!("(?{prefix}){body}")
    };
    regex::Regex::new(&full)
        .map_err(|e| Error::Sqlstate("2201B", format!("invalid regular expression: {e}")))
}

// ------------------------------------------------------------------------
// The SQL surface
// ------------------------------------------------------------------------

/// What a `jsonb_path_*` call returns.
pub enum PathOut {
    Items(Vec<Json>),
    Bool(Option<bool>),
}

/// `executeJsonPath` for `jsonb_path_query` (`want_items`) or
/// `jsonb_path_exists`. `Err` is a raised error; `Ok(None)` is jperError
/// under `silent`.
fn execute(
    path: &JsonPath,
    vars: &Json,
    target: &Json,
    throw_errors: bool,
    want_items: bool,
) -> Result<Option<(Res, Vec<Json>)>> {
    if !matches!(vars, Json::Object(_)) {
        return Err(sqlerr("22023", "\"vars\" argument is not an object"));
    }
    let Some(expr) = &path.expr else {
        return Ok(Some((Res::NotFound, Vec::new())));
    };
    let mut cxt = Cxt {
        vars,
        root: target,
        current: target.clone(),
        lax: path.lax,
        ignore_structural: path.lax,
        throw_errors,
        innermost_array_size: -1,
        pending_path: Some(Vec::new()),
        current_path: Some(Vec::new()),
    };
    let mut found = Vec::new();
    let r = if !want_items && cxt.strict_absence_of_errors() {
        let r = cxt.exec(expr, target, Some(&mut found))?;
        if r == Res::Error {
            r
        } else if found.is_empty() {
            Res::NotFound
        } else {
            Res::Ok
        }
    } else if want_items {
        cxt.exec(expr, target, Some(&mut found))?
    } else {
        cxt.exec(expr, target, None)?
    };
    if r == Res::Error {
        return Ok(None);
    }
    Ok(Some((r, found)))
}

fn parse_json(v: &str) -> Result<Json> {
    crate::json::parse(v).map_err(|_| {
        sqlerr(
            "22P02",
            format!("invalid input syntax for type json: \"{v}\""),
        )
    })
}

/// Evaluate a `jsonb_path_*` function over its SQL arguments' texts.
pub fn call(
    name: &str,
    target: &str,
    path: &str,
    vars: Option<&str>,
    silent: bool,
) -> Result<PathOut> {
    let target = parse_json(target)?;
    let path = parse(path)?;
    let vars = match vars {
        Some(v) => parse_json(v)?,
        None => Json::Object(Vec::new()),
    };
    // `*_tz` lets a datetime comparison cross the time-zone line.
    USE_TZ.with(|t| t.set(name.ends_with("_tz")));
    DT_ERROR.with(|c| c.borrow_mut().take());
    let out = call_inner(name.trim_end_matches("_tz"), &target, &path, &vars, silent);
    // A comparison refused inside a filter is the statement's error (it
    // is not one lax mode or `silent` suppresses).
    if let Some(e) = DT_ERROR.with(|c| c.borrow_mut().take()) {
        return Err(e);
    }
    // A datetime item leaves the path as the string it prints as.
    out.map(|o| match o {
        PathOut::Items(items) => PathOut::Items(items.into_iter().map(dt_to_json).collect()),
        other => other,
    })
}

fn call_inner(
    name: &str,
    target: &Json,
    path: &JsonPath,
    vars: &Json,
    silent: bool,
) -> Result<PathOut> {
    let (target, path, vars) = (target, path, vars);
    match name {
        "jsonb_path_exists" => Ok(PathOut::Bool(
            execute(path, vars, target, !silent, false)?.map(|(r, _)| r == Res::Ok),
        )),
        "jsonb_path_match" => {
            let Some((_, found)) = execute(path, vars, target, !silent, true)? else {
                return Ok(PathOut::Bool(None));
            };
            match found.as_slice() {
                [Json::Bool(b)] => Ok(PathOut::Bool(Some(*b))),
                [Json::Null] => Ok(PathOut::Bool(None)),
                _ if silent => Ok(PathOut::Bool(None)),
                _ => Err(sqlerr("22038", "single boolean result is expected")),
            }
        }
        _ => {
            let found = execute(path, vars, target, !silent, true)?
                .map(|(_, f)| f)
                .unwrap_or_default();
            Ok(PathOut::Items(found))
        }
    }
}

/// Is `name` one of the SQL/JSON path functions?
pub fn is_function(name: &str) -> bool {
    matches!(
        name,
        "jsonb_path_exists"
            | "jsonb_path_match"
            | "jsonb_path_query"
            | "jsonb_path_query_array"
            | "jsonb_path_query_first"
            | "jsonb_path_exists_tz"
            | "jsonb_path_match_tz"
            | "jsonb_path_query_tz"
            | "jsonb_path_query_array_tz"
            | "jsonb_path_query_first_tz"
    )
}

// ------------------------------------------------------------------------
// .datetime()
// ------------------------------------------------------------------------

thread_local! {
    static USE_TZ: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static DT_ERROR: std::cell::RefCell<Option<Error>> = const { std::cell::RefCell::new(None) };
}

const DT_KEY: &str = "\u{0}datetime";

/// A datetime item: its kind, the text it prints as, and a comparison key
/// (micros since the epoch -- LOCAL for the zone-less kinds, UTC for the
/// zoned ones; micros of the day for the times).
struct Dt {
    kind: String,
    text: String,
    key: i64,
}

fn dt_json(kind: &str, text: String, key: i64, offset: Option<i64>) -> Json {
    Json::Object(vec![(
        DT_KEY.to_string(),
        Json::Array(vec![
            Json::Str(kind.to_string()),
            Json::Str(text),
            Json::Number(key.to_string()),
            offset.map_or(Json::Null, |o| Json::Number(o.to_string())),
        ]),
    )])
}

fn dt_of(v: &Json) -> Option<Dt> {
    let Json::Object(o) = v else { return None };
    let [(k, Json::Array(parts))] = o.as_slice() else {
        return None;
    };
    if k != DT_KEY {
        return None;
    }
    match parts.as_slice() {
        [Json::Str(kind), Json::Str(text), Json::Number(key), _] => Some(Dt {
            kind: kind.clone(),
            text: text.clone(),
            key: key.parse().ok()?,
        }),
        _ => None,
    }
}

fn dt_to_json(v: Json) -> Json {
    match dt_of(&v) {
        Some(d) => Json::Str(d.text),
        None => v,
    }
}

fn tri_of(op: Op, ord: std::cmp::Ordering) -> Tri {
    let r = match op {
        Op::Eq => ord.is_eq(),
        Op::Ne => ord.is_ne(),
        Op::Lt => ord.is_lt(),
        Op::Gt => ord.is_gt(),
        Op::Le => ord.is_le(),
        Op::Ge => ord.is_ge(),
        _ => false,
    };
    if r {
        Tri::True
    } else {
        Tri::False
    }
}

const USECS_PER_DAY: i64 = 86_400_000_000;

/// Compare two datetime items (`compareDatetime`): a date against a
/// timestamp as timestamps; a zone-less kind against a zoned one only under
/// `*_tz` (read in UTC, the session zone this server defaults to); a date or
/// timestamp against a time never (UNKNOWN).
fn compare_datetimes(a: &Dt, b: &Dt) -> Result<Option<std::cmp::Ordering>> {
    let family = |k: &str| match k {
        "date" | "timestamp" | "timestamptz" => 1,
        _ => 2,
    };
    if family(&a.kind) != family(&b.kind) {
        return Ok(None);
    }
    let zoned = |k: &str| matches!(k, "timestamptz" | "timetz");
    if zoned(&a.kind) != zoned(&b.kind) && !USE_TZ.with(|t| t.get()) {
        let (from, to) = if zoned(&a.kind) {
            (&b.kind, &a.kind)
        } else {
            (&a.kind, &b.kind)
        };
        let mut msg = format!("cannot convert value from {from} to {to} without time zone usage");
        msg.push_str("\nHint: Use *_tz() function for time zone support.");
        return Err(sqlerr("0A000", msg));
    }
    // Zone-less keys are local; with `*_tz` they are read as UTC.
    Ok(Some(a.key.cmp(&b.key)))
}

fn two(n: i64) -> String {
    format!("{n:02}")
}

fn frac_text(us: i64) -> String {
    if us == 0 {
        return String::new();
    }
    let s = format!(".{us:06}");
    s.trim_end_matches('0').to_string()
}

fn time_text(tod: i64) -> String {
    let secs = tod / 1_000_000;
    format!(
        "{}:{}:{}{}",
        two(secs / 3600),
        two(secs / 60 % 60),
        two(secs % 60),
        frac_text(tod % 1_000_000)
    )
}

fn offset_text(off: i64) -> String {
    let sign = if off < 0 { '-' } else { '+' };
    let a = off.abs();
    let (h, m, s) = (a / 3600, a / 60 % 60, a % 60);
    if s != 0 {
        format!("{sign}{}:{}:{}", two(h), two(m), two(s))
    } else {
        format!("{sign}{}:{}", two(h), two(m))
    }
}

fn date_text(d: chrono::NaiveDate) -> String {
    use chrono::Datelike;
    format!(
        "{:04}-{}-{}",
        d.year(),
        two(i64::from(d.month())),
        two(i64::from(d.day()))
    )
}

fn days_from_epoch(d: chrono::NaiveDate) -> i64 {
    (d - chrono::NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch")).num_days()
}

/// Build the item for a parsed value of `kind`.
fn dt_item(kind: &str, date: Option<chrono::NaiveDate>, tod: i64, off: Option<i64>) -> Json {
    match kind {
        "date" => {
            let d = date.expect("date kind");
            dt_json(
                "date",
                date_text(d),
                days_from_epoch(d) * USECS_PER_DAY,
                None,
            )
        }
        "time" => dt_json("time", time_text(tod), tod, None),
        "timetz" => {
            let o = off.unwrap_or(0);
            dt_json(
                "timetz",
                format!("{}{}", time_text(tod), offset_text(o)),
                tod - o * 1_000_000,
                Some(o),
            )
        }
        "timestamp" => {
            let d = date.expect("date");
            dt_json(
                "timestamp",
                format!("{}T{}", date_text(d), time_text(tod)),
                days_from_epoch(d) * USECS_PER_DAY + tod,
                None,
            )
        }
        _ => {
            let d = date.expect("date");
            let o = off.unwrap_or(0);
            dt_json(
                "timestamptz",
                format!("{}T{}{}", date_text(d), time_text(tod), offset_text(o)),
                days_from_epoch(d) * USECS_PER_DAY + tod - o * 1_000_000,
                Some(o),
            )
        }
    }
}

/// `[+-]HH[:MM[:SS]]` at the end of a time, if any: `(rest, offset secs)`.
fn split_offset(s: &str) -> (&str, Option<i64>) {
    let Some(pos) = s.rfind(['+', '-']) else {
        return (s, None);
    };
    let (head, tail) = s.split_at(pos);
    let sign = if tail.starts_with('-') { -1 } else { 1 };
    let parts: Vec<&str> = tail[1..].split(':').collect();
    let nums: Option<Vec<i64>> = parts.iter().map(|p| p.parse::<i64>().ok()).collect();
    match nums.as_deref() {
        Some([h]) if parts[0].len() <= 2 => (head, Some(sign * h * 3600)),
        Some([h, m]) => (head, Some(sign * (h * 3600 + m * 60))),
        Some([h, m, sec]) => (head, Some(sign * (h * 3600 + m * 60 + sec))),
        _ => (s, None),
    }
}

fn parse_time_of_day(s: &str) -> Option<i64> {
    let (hms, frac) = match s.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (s, None),
    };
    let parts: Vec<&str> = hms.split(':').collect();
    let [h, m, sec] = parts.as_slice() else {
        return None;
    };
    if h.len() != 2 || m.len() != 2 || sec.len() != 2 {
        return None;
    }
    let (h, m, sec): (i64, i64, i64) = (h.parse().ok()?, m.parse().ok()?, sec.parse().ok()?);
    if h > 24 || m > 59 || sec > 60 {
        return None;
    }
    let us = match frac {
        Some(f) if !f.is_empty() && f.chars().all(|c| c.is_ascii_digit()) => {
            let padded: String = f.chars().chain(std::iter::repeat('0')).take(6).collect();
            padded.parse::<i64>().ok()?
        }
        Some(_) => return None,
        None => 0,
    };
    Some(((h * 60 + m) * 60 + sec) * 1_000_000 + us)
}

fn parse_date(s: &str) -> Option<chrono::NaiveDate> {
    let parts: Vec<&str> = s.split('-').collect();
    let [y, m, d] = parts.as_slice() else {
        return None;
    };
    if y.len() < 4 || m.len() != 2 || d.len() != 2 {
        return None;
    }
    chrono::NaiveDate::from_ymd_opt(y.parse().ok()?, m.parse().ok()?, d.parse().ok()?)
}

/// `.datetime([template])` (`executeDateTimeMethod`): the ISO forms
/// PostgreSQL tries without a template -- date, time, time with zone,
/// timestamp (space or `T`), timestamp with zone -- or the template's
/// fields.
fn parse_datetime(text: &str, template: Option<&str>) -> Result<Json> {
    if let Some(fmt) = template {
        let (d, tod, off) = crate::datetime::from_char_strict(text, fmt)?;
        let f = fmt.to_ascii_uppercase();
        let has_date = ["YY", "MM", "DD", "MON", "J", "IYY", "Q"]
            .iter()
            .any(|t| f.contains(t));
        let has_time = ["HH", "MI", "SS", "MS", "US", "AM", "PM"]
            .iter()
            .any(|t| f.contains(t));
        let has_tz = ["TZH", "TZM", "TZ", "OF"].iter().any(|t| f.contains(t));
        let kind = match (has_date, has_time, has_tz) {
            (true, false, _) => "date",
            (false, _, false) => "time",
            (false, _, true) => "timetz",
            (true, true, false) => "timestamp",
            (true, true, true) => "timestamptz",
        };
        return Ok(dt_item(kind, Some(d), tod, off));
    }
    let unrecognised = || {
        sqlerr(
            "22031",
            format!(
                "datetime format is not recognized: \"{text}\"\nHint: Use a datetime template argument to specify the input data format."
            ),
        )
    };
    let t = text.trim();
    if let Some(d) = parse_date(t) {
        return Ok(dt_item("date", Some(d), 0, None));
    }
    // Time, with or without a zone.
    let (time_part, off) = split_offset(t);
    if let Some(tod) = parse_time_of_day(time_part) {
        return Ok(dt_item(
            if off.is_some() { "timetz" } else { "time" },
            None,
            tod,
            off,
        ));
    }
    // Timestamp: a date, a space or `T`, a time, maybe a zone.
    if t.len() > 11 {
        let (date_part, rest) = t.split_at(10);
        let sep = rest.chars().next();
        if matches!(sep, Some(' ' | 'T')) {
            if let Some(d) = parse_date(date_part) {
                let (time_part, off) = split_offset(&rest[1..]);
                if let Some(tod) = parse_time_of_day(time_part) {
                    let kind = if off.is_some() {
                        "timestamptz"
                    } else {
                        "timestamp"
                    };
                    return Ok(dt_item(kind, Some(d), tod, off));
                }
            }
        }
    }
    Err(unrecognised())
}
