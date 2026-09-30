//! The `xml` type: a value is its text, checked for well-formedness on the
//! way in, as PostgreSQL's `xml_in` does, and built by the SQL/XML
//! constructors (`xmlelement`, `xmlforest`, `xmlconcat`, `xmlpi`,
//! `xmlroot`, `xmlcomment`).
//!
//! PostgreSQL hands the checking to libxml2; this is a well-formedness
//! parser for the XML 1.0 grammar those calls reach -- elements, attributes,
//! character and entity references, comments, processing instructions,
//! CDATA, the XML declaration and a DOCTYPE with its internal subset. The
//! error CODES match; the DETAIL text is libxml2's and is not reproduced.
//!
//! Every escaping and declaration rule here was measured against PostgreSQL
//! 14: content escapes `&` `<` `>` and `\r`; an attribute value also escapes
//! `"`, tab and newline (libxml2's writer); `print_xml_decl` prints a
//! declaration only when it says something beyond `version="1.0"`.

use crate::{Error, Result};

/// `XMLOPTION`: whether a value must be a single-rooted DOCUMENT or may be
/// any well-formed CONTENT (the session default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XmlOption {
    Document,
    Content,
}

fn invalid(option: XmlOption, detail: &str) -> Error {
    let _ = detail;
    match option {
        XmlOption::Document => Error::Sqlstate("2200M", "invalid XML document".into()),
        XmlOption::Content => Error::Sqlstate("2200N", "invalid XML content".into()),
    }
}

/// `xml_in` / `XMLPARSE`: check `text` and return it unchanged.
pub fn parse(text: &str, option: XmlOption) -> Result<String> {
    check(text, option).map_err(|d| invalid(option, &d))?;
    Ok(text.to_string())
}

/// Whether `text` is well-formed under `option` (`xml_is_well_formed*`).
pub fn is_well_formed(text: &str, option: XmlOption) -> bool {
    check(text, option).is_ok()
}

/// `x IS DOCUMENT`.
pub fn is_document(text: &str) -> bool {
    check(text, XmlOption::Document).is_ok()
}

fn check(text: &str, option: XmlOption) -> std::result::Result<(), String> {
    // CONTENT that carries a DOCTYPE is parsed as a document (PostgreSQL 12+,
    // `xml_doctype_in_content`).
    let document = option == XmlOption::Document || doctype_in_content(text);
    let mut p = Parser {
        s: text.chars().collect(),
        i: 0,
        entities: Vec::new(),
    };
    p.xml_decl()?;
    if document {
        p.document()
    } else {
        p.content_top()
    }
}

fn doctype_in_content(text: &str) -> bool {
    let (_, rest) = split_decl(text);
    let mut rest = rest;
    loop {
        rest = rest.trim_start_matches([' ', '\t', '\r', '\n']);
        if let Some(r) = rest.strip_prefix("<!--") {
            match r.find("-->") {
                Some(end) => rest = &r[end + 3..],
                None => return false,
            }
        } else if let Some(r) = rest.strip_prefix("<?") {
            match r.find("?>") {
                Some(end) => rest = &r[end + 2..],
                None => return false,
            }
        } else {
            return rest.starts_with("<!DOCTYPE");
        }
    }
}

struct Parser {
    s: Vec<char>,
    i: usize,
    /// Entities the internal DTD subset declares.
    entities: Vec<String>,
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == ':' || !c.is_ascii()
}

fn is_name_char(c: char) -> bool {
    is_name_start(c) || c.is_ascii_digit() || c == '-' || c == '.'
}

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

type PResult<T> = std::result::Result<T, String>;

impl Parser {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn starts(&self, lit: &str) -> bool {
        lit.chars()
            .enumerate()
            .all(|(k, c)| self.s.get(self.i + k) == Some(&c))
    }

    fn eat(&mut self, lit: &str) -> bool {
        if self.starts(lit) {
            self.i += lit.chars().count();
            true
        } else {
            false
        }
    }

    fn skip_space(&mut self) -> bool {
        let start = self.i;
        while self.peek().is_some_and(is_space) {
            self.i += 1;
        }
        self.i > start
    }

    fn name(&mut self) -> PResult<String> {
        match self.peek() {
            Some(c) if is_name_start(c) => {}
            _ => return Err("name expected".into()),
        }
        let start = self.i;
        while self.peek().is_some_and(is_name_char) {
            self.i += 1;
        }
        Ok(self.s[start..self.i].iter().collect())
    }

    /// `<?xml version="..." [encoding="..."] [standalone="..."]?>` at the
    /// very start.
    fn xml_decl(&mut self) -> PResult<()> {
        if !(self.starts("<?xml") && self.s.get(self.i + 5).is_some_and(|c| is_space(*c))) {
            return Ok(());
        }
        self.i += 5;
        loop {
            self.skip_space();
            if self.eat("?>") {
                return Ok(());
            }
            let _ = self.name()?;
            self.skip_space();
            if !self.eat("=") {
                return Err("'=' expected in the XML declaration".into());
            }
            self.skip_space();
            self.quoted()?;
        }
    }

    fn quoted(&mut self) -> PResult<String> {
        let q = match self.peek() {
            Some(q @ ('"' | '\'')) => q,
            _ => return Err("quoted value expected".into()),
        };
        self.i += 1;
        let start = self.i;
        while let Some(c) = self.peek() {
            if c == q {
                let v: String = self.s[start..self.i].iter().collect();
                self.i += 1;
                return Ok(v);
            }
            self.i += 1;
        }
        Err("unterminated quoted value".into())
    }

    fn document(&mut self) -> PResult<()> {
        let mut root = false;
        let mut doctype = false;
        loop {
            self.skip_space();
            if self.peek().is_none() {
                return if root {
                    Ok(())
                } else {
                    Err("Start tag expected, '<' not found".into())
                };
            }
            if self.starts("<!--") {
                self.comment()?;
            } else if self.starts("<?") {
                self.pi()?;
            } else if self.starts("<!DOCTYPE") && !root && !doctype {
                self.doctype()?;
                doctype = true;
            } else if self.peek() == Some('<') && !root {
                self.element()?;
                root = true;
            } else {
                return Err(if root {
                    "Extra content at the end of the document".into()
                } else {
                    "Start tag expected, '<' not found".into()
                });
            }
        }
    }

    /// Top-level CONTENT: text, elements, comments, PIs and CDATA, in any
    /// number.
    fn content_top(&mut self) -> PResult<()> {
        self.content(None)
    }

    /// Element content up to `</end>` (or the end of input at top level).
    fn content(&mut self, end: Option<&str>) -> PResult<()> {
        loop {
            match self.peek() {
                None => {
                    return match end {
                        None => Ok(()),
                        Some(tag) => Err(format!("Premature end of data in tag {tag}")),
                    };
                }
                Some('<') => {
                    if self.starts("</") {
                        let Some(tag) = end else {
                            return Err("unexpected end tag".into());
                        };
                        self.i += 2;
                        let name = self.name()?;
                        if name != tag {
                            return Err(format!(
                                "Opening and ending tag mismatch: {tag} and {name}"
                            ));
                        }
                        self.skip_space();
                        if !self.eat(">") {
                            return Err("'>' expected".into());
                        }
                        return Ok(());
                    } else if self.starts("<!--") {
                        self.comment()?;
                    } else if self.starts("<![CDATA[") {
                        self.i += 9;
                        loop {
                            if self.peek().is_none() {
                                return Err("CData section not finished".into());
                            }
                            if self.eat("]]>") {
                                break;
                            }
                            self.i += 1;
                        }
                    } else if self.starts("<?") {
                        self.pi()?;
                    } else {
                        self.element()?;
                    }
                }
                Some('&') => self.reference()?,
                Some(_) => {
                    if self.starts("]]>") {
                        return Err("Sequence ']]>' not allowed in content".into());
                    }
                    self.i += 1;
                }
            }
        }
    }

    fn element(&mut self) -> PResult<()> {
        if !self.eat("<") {
            return Err("'<' expected".into());
        }
        let tag = self.name()?;
        let mut seen: Vec<String> = Vec::new();
        loop {
            let spaced = self.skip_space();
            if self.eat("/>") {
                return Ok(());
            }
            if self.eat(">") {
                return self.content(Some(&tag));
            }
            if !spaced {
                return Err("attributes construct error".into());
            }
            let attr = self.name()?;
            if seen.contains(&attr) {
                return Err(format!("Attribute {attr} redefined"));
            }
            seen.push(attr);
            self.skip_space();
            if !self.eat("=") {
                return Err("Specification mandates value for attribute".into());
            }
            self.skip_space();
            let q = match self.peek() {
                Some(q @ ('"' | '\'')) => q,
                _ => return Err("AttValue: \" or ' expected".into()),
            };
            self.i += 1;
            loop {
                match self.peek() {
                    None => return Err("AttValue: ' expected".into()),
                    Some(c) if c == q => {
                        self.i += 1;
                        break;
                    }
                    Some('<') => {
                        return Err("Unescaped '<' not allowed in attributes values".into())
                    }
                    Some('&') => self.reference()?,
                    Some(_) => self.i += 1,
                }
            }
        }
    }

    /// `&name;`, `&#n;` or `&#xh;`.
    fn reference(&mut self) -> PResult<()> {
        self.i += 1;
        if self.eat("#") {
            let hex = self.eat("x");
            let start = self.i;
            while self.peek().is_some_and(|c| {
                if hex {
                    c.is_ascii_hexdigit()
                } else {
                    c.is_ascii_digit()
                }
            }) {
                self.i += 1;
            }
            let digits: String = self.s[start..self.i].iter().collect();
            let code = u32::from_str_radix(&digits, if hex { 16 } else { 10 })
                .map_err(|_| "invalid character reference".to_string())?;
            let valid = matches!(code, 0x9 | 0xA | 0xD)
                || (0x20..=0xD7FF).contains(&code)
                || (0xE000..=0xFFFD).contains(&code)
                || (0x10000..=0x10FFFF).contains(&code);
            if !valid || !self.eat(";") {
                return Err("invalid character reference".into());
            }
            return Ok(());
        }
        let name = self
            .name()
            .map_err(|_| "xmlParseEntityRef: no name".to_string())?;
        if !self.eat(";") {
            return Err(format!("EntityRef: expecting ';' after {name}"));
        }
        if matches!(name.as_str(), "amp" | "lt" | "gt" | "quot" | "apos")
            || self.entities.contains(&name)
        {
            Ok(())
        } else {
            Err(format!("Entity '{name}' not defined"))
        }
    }

    fn comment(&mut self) -> PResult<()> {
        self.i += 4;
        loop {
            if self.peek().is_none() {
                return Err("Comment not terminated".into());
            }
            if self.starts("--") {
                return if self.eat("-->") {
                    Ok(())
                } else {
                    Err("Double hyphen within comment".into())
                };
            }
            self.i += 1;
        }
    }

    fn pi(&mut self) -> PResult<()> {
        self.i += 2;
        let target = self.name()?;
        if target.eq_ignore_ascii_case("xml") {
            return Err("XML declaration allowed only at the start of the document".into());
        }
        if self.eat("?>") {
            return Ok(());
        }
        if !self.skip_space() {
            return Err("ParsePI: PI space expected".into());
        }
        loop {
            if self.peek().is_none() {
                return Err("PI not terminated".into());
            }
            if self.eat("?>") {
                return Ok(());
            }
            self.i += 1;
        }
    }

    /// `<!DOCTYPE name [external id] [internal subset]>`, recording the
    /// general entities the internal subset declares.
    fn doctype(&mut self) -> PResult<()> {
        self.i += "<!DOCTYPE".len();
        if !self.skip_space() {
            return Err("Space required after '<!DOCTYPE'".into());
        }
        let _ = self.name()?;
        loop {
            self.skip_space();
            match self.peek() {
                None => return Err("DOCTYPE not terminated".into()),
                Some('>') => {
                    self.i += 1;
                    return Ok(());
                }
                Some('"' | '\'') => {
                    self.quoted()?;
                }
                Some('[') => {
                    self.i += 1;
                    self.internal_subset()?;
                }
                Some(_) => {
                    let _ = self.name()?;
                }
            }
        }
    }

    fn internal_subset(&mut self) -> PResult<()> {
        loop {
            self.skip_space();
            match self.peek() {
                None => return Err("internal subset not terminated".into()),
                Some(']') => {
                    self.i += 1;
                    return Ok(());
                }
                _ if self.starts("<!--") => self.comment()?,
                _ if self.starts("<?") => self.pi()?,
                _ if self.starts("<!ENTITY") => {
                    self.i += "<!ENTITY".len();
                    self.skip_space();
                    let parameter = self.eat("%");
                    self.skip_space();
                    let name = self.name()?;
                    if !parameter {
                        self.entities.push(name);
                    }
                    self.declaration_rest()?;
                }
                _ if self.starts("<!") => {
                    self.i += 2;
                    self.declaration_rest()?;
                }
                Some('%') => {
                    // A parameter-entity reference.
                    self.i += 1;
                    let _ = self.name()?;
                    if !self.eat(";") {
                        return Err("PEReference: expecting ';'".into());
                    }
                }
                Some(_) => return Err("internal subset: unexpected character".into()),
            }
        }
    }

    /// The rest of a markup declaration, through its `>`, honouring quotes.
    fn declaration_rest(&mut self) -> PResult<()> {
        loop {
            match self.peek() {
                None => return Err("declaration not terminated".into()),
                Some('>') => {
                    self.i += 1;
                    return Ok(());
                }
                Some('"' | '\'') => {
                    self.quoted()?;
                }
                Some(_) => self.i += 1,
            }
        }
    }
}

// ------------------------------------------------------------------------
// Escaping and names
// ------------------------------------------------------------------------

/// `escape_xml`: element content.
pub fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\r' => out.push_str("&#x0d;"),
            c => out.push(c),
        }
    }
    out
}

/// libxml2's attribute-value escaping (`xmlTextWriterWriteAttribute`).
pub fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' => out.push_str("&#9;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            c => out.push(c),
        }
    }
    out
}

/// `map_sql_identifier_to_xml_name(ident, false, false)`: a character that
/// cannot stand in an XML name becomes `_xHHHH_`; so does a leading `:` and
/// the `_` of a literal `_x`.
pub fn xml_name(ident: &str) -> String {
    let chars: Vec<char> = ident.chars().collect();
    let mut out = String::new();
    for (i, c) in chars.iter().copied().enumerate() {
        let first = i == 0;
        if c == ':' && first {
            out.push_str("_x003A_");
        } else if c == '_' && chars.get(i + 1) == Some(&'x') {
            out.push_str("_x005F_");
        } else if !(if first {
            is_name_start(c) && c != ':'
        } else {
            is_name_char(c)
        }) {
            out.push_str(&format!("_x{:04X}_", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

// ------------------------------------------------------------------------
// Declarations
// ------------------------------------------------------------------------

/// A leading XML declaration's `(version, standalone)` and the text after
/// it; `standalone` is 1 for yes, 0 for no, -1 when absent. `None` for the
/// declaration when there is none.
struct Decl {
    version: Option<String>,
    standalone: i32,
}

fn split_decl(text: &str) -> (Option<Decl>, &str) {
    let is_decl = text.starts_with("<?xml") && text[5..].chars().next().is_some_and(is_space);
    if !is_decl {
        return (None, text);
    }
    let Some(end) = text.find("?>") else {
        return (None, text);
    };
    let body = &text[5..end];
    let attr = |name: &str| -> Option<String> {
        let at = body.find(name)?;
        let rest = body[at + name.len()..].trim_start();
        let rest = rest.strip_prefix('=')?.trim_start();
        let q = rest.chars().next()?;
        let inner = &rest[1..];
        let close = inner.find(q)?;
        Some(inner[..close].to_string())
    };
    let standalone = match attr("standalone").as_deref() {
        Some("yes") => 1,
        Some("no") => 0,
        _ => -1,
    };
    (
        Some(Decl {
            version: attr("version"),
            standalone,
        }),
        &text[end + 2..],
    )
}

/// `print_xml_decl`: only a declaration that says something beyond the
/// default version is printed.
fn print_decl(version: Option<&str>, standalone: i32) -> String {
    if version.is_some_and(|v| v != "1.0") || standalone != -1 {
        let mut out = format!("<?xml version=\"{}\"", version.unwrap_or("1.0"));
        match standalone {
            1 => out.push_str(" standalone=\"yes\""),
            0 => out.push_str(" standalone=\"no\""),
            _ => {}
        }
        out.push_str("?>");
        out
    } else {
        String::new()
    }
}

/// `XMLCONCAT`: the non-NULL values, their declarations merged into one.
pub fn concat(values: &[String]) -> String {
    let mut global_standalone = 1;
    let mut global_version: Option<String> = None;
    let mut version_no_value = false;
    let mut body = String::new();
    for v in values {
        let (decl, rest) = split_decl(v);
        let (version, standalone) = match decl {
            Some(d) => (d.version, d.standalone),
            None => (None, -1),
        };
        if standalone == 0 && global_standalone == 1 {
            global_standalone = 0;
        }
        if standalone < 0 {
            global_standalone = -1;
        }
        match version {
            None => version_no_value = true,
            Some(v) => match &global_version {
                None => global_version = Some(v),
                Some(g) if *g != v => version_no_value = true,
                Some(_) => {}
            },
        }
        body.push_str(rest);
    }
    if !version_no_value || global_standalone >= 0 {
        let version = if version_no_value {
            None
        } else {
            global_version.as_deref()
        };
        format!("{}{body}", print_decl(version, global_standalone))
    } else {
        body
    }
}

/// `XMLROOT(x, VERSION v | NO VALUE [, STANDALONE ...])`. `standalone` is
/// `Some(1 | 0 | -1)` for YES / NO / NO VALUE, `None` when omitted (the
/// value's own is kept).
pub fn root(text: &str, version: Option<&str>, standalone: Option<i32>) -> String {
    let (decl, rest) = split_decl(text);
    let orig_standalone = decl.map_or(-1, |d| d.standalone);
    let standalone = standalone.unwrap_or(orig_standalone);
    format!("{}{rest}", print_decl(version, standalone))
}

/// `xmlcomment(text)`.
pub fn comment(text: &str) -> Result<String> {
    if text.contains("--") || text.ends_with('-') {
        return Err(Error::Sqlstate("2200S", "invalid XML comment".into()));
    }
    Ok(format!("<!--{text}-->"))
}

/// `XMLPI(NAME target [, content])`.
pub fn pi(target: &str, content: Option<&str>) -> Result<String> {
    // Refused at parse analysis in PostgreSQL, hence a syntax-class code.
    if target.eq_ignore_ascii_case("xml") {
        return Err(Error::Sqlstate(
            "42601",
            "invalid XML processing instruction".into(),
        ));
    }
    let target = xml_name(target);
    match content {
        None => Ok(format!("<?{target}?>")),
        Some(c) => {
            if c.contains("?>") {
                return Err(Error::Sqlstate(
                    "2200T",
                    "invalid XML processing instruction".into(),
                ));
            }
            let c = c.trim_start_matches(' ');
            if c.is_empty() {
                Ok(format!("<?{target}?>"))
            } else {
                Ok(format!("<?{target} {c}?>"))
            }
        }
    }
}

/// `XMLSERIALIZE(DOCUMENT x AS text)` refuses a value that is not one.
pub fn serialize(text: &str, option: XmlOption) -> Result<String> {
    if option == XmlOption::Document && !is_document(text) {
        return Err(Error::Sqlstate("2200L", "not an XML document".into()));
    }
    Ok(text.to_string())
}

// ------------------------------------------------------------------------
// Functions
// ------------------------------------------------------------------------

const FUNCTIONS: &[&str] = &[
    "xpath",
    "xpath_exists",
    "xmlexists",
    "xmlcomment",
    "xml_is_well_formed",
    "xml_is_well_formed_document",
    "xml_is_well_formed_content",
];

pub fn is_function(name: &str) -> bool {
    FUNCTIONS.contains(&name)
}

pub fn result_type(name: &str) -> Option<&'static str> {
    Some(match name {
        "xmlcomment" => "xml",
        "xpath" => "xml[]",
        "xpath_exists" | "xmlexists" => "bool",
        "xml_is_well_formed" | "xml_is_well_formed_document" | "xml_is_well_formed_content" => {
            "bool"
        }
        _ => return None,
    })
}

/// The `xml` scalar functions; `None` when `name` is not one.
pub fn call(name: &str, args: &[bson::Bson]) -> Option<Result<bson::Bson>> {
    use bson::Bson;
    if !is_function(name) {
        return None;
    }
    if matches!(name, "xpath" | "xpath_exists" | "xmlexists") {
        return Some(xpath_call(name, args));
    }
    if args.len() != 1 {
        return Some(Err(Error::UndefinedFunction(format!(
            "function {name} does not exist"
        ))));
    }
    if args[0] == Bson::Null {
        return Some(Ok(Bson::Null));
    }
    let text = crate::value_text(&args[0]);
    Some(Ok(match name {
        "xmlcomment" => match comment(&text) {
            Ok(c) => Bson::String(c),
            Err(e) => return Some(Err(e)),
        },
        // `xml_is_well_formed` follows XMLOPTION, CONTENT by default.
        "xml_is_well_formed" | "xml_is_well_formed_content" => {
            Bson::Boolean(is_well_formed(&text, XmlOption::Content))
        }
        "xml_is_well_formed_document" => Bson::Boolean(is_well_formed(&text, XmlOption::Document)),
        _ => return None,
    }))
}

/// `xml_out`: a value's leading declaration is re-printed by
/// `print_xml_decl`, so the default `<?xml version="1.0"?>` disappears (with
/// one newline after it); the rest is the text as stored. A cast to `text`
/// does not go through here -- it keeps the text verbatim.
pub fn out(text: &str) -> String {
    let (decl, rest) = split_decl(text);
    let Some(d) = decl else {
        return text.to_string();
    };
    let printed = print_decl(d.version.as_deref(), d.standalone);
    if printed.is_empty() {
        rest.strip_prefix('\n').unwrap_or(rest).to_string()
    } else {
        format!("{printed}{rest}")
    }
}

// ------------------------------------------------------------------------
// XPath
// ------------------------------------------------------------------------

/// Text content as libxml2's node dump escapes it.
fn dump_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\r' => out.push_str("&#13;"),
            c => out.push(c),
        }
    }
    out
}

fn qualified(prefix: Option<&str>, local: &str) -> String {
    match prefix {
        Some(p) if !p.is_empty() => format!("{p}:{local}"),
        _ => local.to_string(),
    }
}

/// `xmlNodeDump` of an element: its own namespace declarations (those not
/// already in scope at its parent), its attributes in document order, and
/// its children -- `<b/>` when it has none.
fn dump_element(e: sxd_document::dom::Element) -> String {
    use sxd_document::dom::{ChildOfElement, ParentOfChild};
    let parent = match e.parent() {
        Some(ParentOfChild::Element(p)) => Some(p),
        _ => None,
    };
    let mut out = format!(
        "<{}",
        qualified(e.preferred_prefix(), e.name().local_part())
    );
    if let Some(uri) = e.default_namespace_uri() {
        if parent.and_then(|p| p.recursive_default_namespace_uri()) != Some(uri) {
            out.push_str(&format!(" xmlns=\"{}\"", escape_attr(uri)));
        }
    }
    let inherited: Vec<(String, String)> = parent
        .map(|p| {
            p.namespaces_in_scope()
                .iter()
                .map(|n| (n.prefix().to_string(), n.uri().to_string()))
                .collect()
        })
        .unwrap_or_default();
    let mut own: Vec<(String, String)> = e
        .namespaces_in_scope()
        .iter()
        .map(|n| (n.prefix().to_string(), n.uri().to_string()))
        .filter(|(p, _)| p != "xml")
        .filter(|pair| !inherited.contains(pair))
        .collect();
    own.sort();
    for (p, uri) in own {
        out.push_str(&format!(" xmlns:{p}=\"{}\"", escape_attr(&uri)));
    }
    for a in e.attributes() {
        out.push_str(&format!(
            " {}=\"{}\"",
            qualified(a.preferred_prefix(), a.name().local_part()),
            escape_attr(a.value())
        ));
    }
    let children = e.children();
    if children.is_empty() {
        out.push_str("/>");
        return out;
    }
    out.push('>');
    for c in children {
        match c {
            ChildOfElement::Element(inner) => out.push_str(&dump_element(inner)),
            ChildOfElement::Text(t) => out.push_str(&dump_text(t.text())),
            ChildOfElement::Comment(c) => out.push_str(&format!("<!--{}-->", c.text())),
            ChildOfElement::ProcessingInstruction(pi) => out.push_str(&match pi.value() {
                Some(v) => format!("<?{} {v}?>", pi.target()),
                None => format!("<?{}?>", pi.target()),
            }),
        }
    }
    out.push_str(&format!(
        "</{}>",
        qualified(e.preferred_prefix(), e.name().local_part())
    ));
    out
}

/// `xmlXPathCastNumberToString`: an integral value without a fraction,
/// `NaN` / `Infinity` spelled out.
fn number_text(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n.is_infinite() {
        if n > 0.0 { "Infinity" } else { "-Infinity" }.into()
    } else if n == n.trunc() && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        let s = format!("{n}");
        s
    }
}

/// PostgreSQL raises these with a bare `elog(ERROR)`, hence XX000.
fn xpath_error(msg: &str) -> Error {
    Error::Sqlstate("XX000", msg.to_string())
}

/// Merge adjacent text children, as libxml2's parser leaves them: sxd
/// splits `a&amp;b` into three text nodes where libxml2 has one.
fn merge_texts(e: sxd_document::dom::Element) {
    use sxd_document::dom::ChildOfElement;
    let mut last: Option<sxd_document::dom::Text> = None;
    for c in e.children() {
        match c {
            ChildOfElement::Text(t) => match last {
                Some(prev) => {
                    prev.set_text(&format!("{}{}", prev.text(), t.text()));
                    t.remove_from_parent();
                }
                None => last = Some(t),
            },
            ChildOfElement::Element(inner) => {
                merge_texts(inner);
                last = None;
            }
            _ => last = None,
        }
    }
}

/// Evaluate `path` over the document `doc` with `namespaces` (prefix, uri)
/// bound, handing each result item to `f`: `xpath()` wants them all,
/// `xpath_exists()` only whether there is one.
fn evaluate(
    path: &str,
    doc: &str,
    namespaces: &[(String, String)],
) -> Result<std::result::Result<Vec<String>, bool>> {
    if path.is_empty() {
        return Err(Error::Sqlstate("22000", "empty XPath expression".into()));
    }
    if !is_document(doc) {
        return Err(Error::Sqlstate(
            "2200M",
            "could not parse XML document".into(),
        ));
    }
    let package = sxd_document::parser::parse(doc)
        .map_err(|_| Error::Sqlstate("2200M", "could not parse XML document".into()))?;
    let document = package.as_document();
    for c in document.root().children() {
        if let sxd_document::dom::ChildOfRoot::Element(e) = c {
            merge_texts(e);
        }
    }
    let factory = sxd_xpath::Factory::new();
    let xpath = factory
        .build(path)
        .map_err(|_| xpath_error("invalid XPath expression"))?
        .ok_or_else(|| xpath_error("invalid XPath expression"))?;
    let mut context = sxd_xpath::Context::new();
    for (prefix, uri) in namespaces {
        context.set_namespace(prefix, uri);
    }
    let value = xpath
        .evaluate(&context, document.root())
        .map_err(|_| xpath_error("could not create XPath object"))?;
    Ok(match value {
        sxd_xpath::Value::Nodeset(nodes) => {
            use sxd_xpath::nodeset::Node;
            Ok(nodes
                .document_order()
                .into_iter()
                .map(|n| match n {
                    Node::Element(e) => dump_element(e),
                    Node::Root(r) => r
                        .children()
                        .into_iter()
                        .map(|c| match c {
                            sxd_document::dom::ChildOfRoot::Element(e) => dump_element(e),
                            sxd_document::dom::ChildOfRoot::Comment(c) => {
                                format!("<!--{}-->", c.text())
                            }
                            sxd_document::dom::ChildOfRoot::ProcessingInstruction(pi) => {
                                match pi.value() {
                                    Some(v) => format!("<?{} {v}?>", pi.target()),
                                    None => format!("<?{}?>", pi.target()),
                                }
                            }
                        })
                        .collect(),
                    Node::Comment(c) => format!("<!--{}-->", c.text()),
                    Node::ProcessingInstruction(pi) => match pi.value() {
                        Some(v) => format!("<?{} {v}?>", pi.target()),
                        None => format!("<?{}?>", pi.target()),
                    },
                    // Attributes and text come back as their escaped value.
                    other => escape_text(&other.string_value()),
                })
                .collect())
        }
        sxd_xpath::Value::Boolean(b) => Err(b),
        sxd_xpath::Value::Number(n) => Ok(vec![number_text(n)]),
        sxd_xpath::Value::String(s) => Ok(vec![escape_text(&s)]),
    })
}

/// `xpath(path, doc [, namespaces])`: every result as an `xml` value.
pub fn xpath(path: &str, doc: &str, namespaces: &[(String, String)]) -> Result<Vec<String>> {
    Ok(match evaluate(path, doc, namespaces)? {
        Ok(items) => items,
        Err(b) => vec![b.to_string()],
    })
}

/// `xpath_exists` / `XMLEXISTS`: whether the path selects anything (a
/// boolean result is its own answer).
pub fn xpath_exists(path: &str, doc: &str, namespaces: &[(String, String)]) -> Result<bool> {
    Ok(match evaluate(path, doc, namespaces)? {
        Ok(items) => !items.is_empty(),
        Err(b) => b,
    })
}

fn xpath_call(name: &str, args: &[bson::Bson]) -> Result<bson::Bson> {
    use bson::Bson;
    if !(2..=3).contains(&args.len()) || (name == "xmlexists" && args.len() != 2) {
        return Err(Error::UndefinedFunction(format!(
            "function {name} does not exist"
        )));
    }
    if args[..2].contains(&Bson::Null) {
        return Ok(Bson::Null);
    }
    let path = crate::value_text(&args[0]);
    let doc = crate::value_text(&args[1]);
    // `ARRAY[ARRAY['prefix', 'uri'], ...]`.
    let mut namespaces = Vec::new();
    if let Some(Bson::Array(pairs)) = args.get(2) {
        for pair in pairs {
            match pair {
                Bson::Array(p) if p.len() == 2 => {
                    namespaces.push((crate::value_text(&p[0]), crate::value_text(&p[1])))
                }
                _ => {
                    return Err(Error::Sqlstate(
                        "22000",
                        "invalid array for XML namespace mapping".into(),
                    ))
                }
            }
        }
    }
    Ok(if name == "xpath" {
        Bson::Array(
            xpath(&path, &doc, &namespaces)?
                .into_iter()
                .map(Bson::String)
                .collect(),
        )
    } else {
        Bson::Boolean(xpath_exists(&path, &doc, &namespaces)?)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_formedness() {
        assert!(is_well_formed("<a><b/></a>", XmlOption::Document));
        assert!(is_well_formed("abc<b/>", XmlOption::Content));
        assert!(!is_well_formed("abc<b/>", XmlOption::Document));
        assert!(!is_well_formed("<a>", XmlOption::Content));
        assert!(!is_well_formed("<a><b></a></b>", XmlOption::Content));
        assert!(!is_well_formed("<a b=\"1\" b=\"2\"/>", XmlOption::Content));
        assert!(!is_well_formed("<a>&foo;</a>", XmlOption::Content));
        assert!(is_well_formed(
            "<!DOCTYPE a [<!ENTITY e \"x\">]><a>&e;</a>",
            XmlOption::Content
        ));
        assert!(is_well_formed("", XmlOption::Content));
    }

    #[test]
    fn names_and_declarations() {
        assert_eq!(xml_name("Foo Bar"), "Foo_x0020_Bar");
        assert_eq!(xml_name("_xa"), "_x005F_xa");
        assert_eq!(xml_name(":a"), "_x003A_a");
        assert_eq!(xml_name("1a"), "_x0031_a");
        assert_eq!(xml_name("a:b"), "a:b");
        assert_eq!(root("<a/>", Some("1.0"), None), "<a/>");
        assert_eq!(
            root("<a/>", Some("1.0"), Some(1)),
            "<?xml version=\"1.0\" standalone=\"yes\"?><a/>"
        );
        assert_eq!(
            concat(&[
                "<?xml version=\"1.1\"?><a/>".into(),
                "<?xml version=\"1.1\" standalone=\"no\"?><b/>".into()
            ]),
            "<?xml version=\"1.1\"?><a/><b/>"
        );
    }
}
