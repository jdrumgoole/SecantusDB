//! The `aclitem` type: `grantee=privileges/grantor`, PostgreSQL's
//! `aclparse` / `aclitemout` (src/backend/utils/adt/acl.c), measured on
//! PostgreSQL 16.
//!
//! The grantee and grantor are ROLES, which PostgreSQL resolves against its
//! role catalog (`role "nobody" does not exist`). This server has no role
//! catalog: the one role it can vouch for is the session user, the name the
//! client gave at startup, which is also the superuser an omitted grantor
//! defaults to (PostgreSQL defaults to the bootstrap superuser, with a
//! WARNING that names its user ID; the message is kept verbatim).

use crate::{Error, Result};

/// PostgreSQL's `ACL_ALL_RIGHTS_STR`: every privilege character, in the
/// order `aclitemout` renders them (the bit order of the privilege mask).
pub const ALL_RIGHTS: &str = "arwdDxtXUCTcsA";

/// The names the server accepts as a grantee or grantor.
pub fn role_exists(name: &str) -> bool {
    crate::session_user().is_some_and(|u| u == name)
}

/// `aclitemin`: parse a literal to its canonical text. An omitted grantor
/// raises PostgreSQL's WARNING (before any error that follows it, as on
/// PostgreSQL, where the warning is raised as the grantor is defaulted).
pub fn parse(input: &str) -> Result<String> {
    let invalid = |m: String| Error::InvalidText(m);
    let (mut name, mut s) = getid(input)?;
    if !s.starts_with('=') {
        // We just read a key word, not a name.
        if name != "group" && name != "user" {
            return Err(invalid(format!(
                "unrecognized key word: \"{name}\"\nHint: ACL key word must be \"group\" or \"user\"."
            )));
        }
        (name, s) = getid(s)?;
        if name.is_empty() {
            return Err(invalid(
                "missing name\nHint: A name must follow the \"group\" or \"user\" key word.".into(),
            ));
        }
    }
    let Some(rest) = s.strip_prefix('=') else {
        return Err(invalid("missing \"=\" sign".into()));
    };
    let mut privs = 0u32;
    let mut goption = 0u32;
    let mut read = 0u32;
    let mut end = rest.len();
    for (i, c) in rest.char_indices() {
        if c == '*' {
            goption |= read;
            read = 0;
        } else if c.is_ascii_alphabetic() {
            let Some(bit) = ALL_RIGHTS.find(c) else {
                return Err(invalid(format!(
                    "invalid mode character: must be one of \"{ALL_RIGHTS}\""
                )));
            };
            read = 1 << bit;
        } else {
            end = i;
            break;
        }
        privs |= read;
    }
    s = &rest[end..];
    if !name.is_empty() && !role_exists(&name) {
        return Err(Error::UndefinedObject(format!(
            "role \"{name}\" does not exist"
        )));
    }
    let grantor = if let Some(after) = s.strip_prefix('/') {
        let (grantor, tail) = getid(after)?;
        if grantor.is_empty() {
            return Err(invalid("a name must follow the \"/\" sign".into()));
        }
        if !role_exists(&grantor) {
            return Err(Error::UndefinedObject(format!(
                "role \"{grantor}\" does not exist"
            )));
        }
        s = tail;
        grantor
    } else {
        // Backward compatibility: the grantor defaults to the superuser.
        crate::warn("0L000", "defaulting grantor to user ID 10".to_string());
        crate::session_user().unwrap_or_default()
    };
    if !s.trim_start().is_empty() {
        return Err(invalid(
            "extra garbage at the end of the ACL specification".into(),
        ));
    }
    let mut out = putid(&name);
    out.push('=');
    for (bit, c) in ALL_RIGHTS.chars().enumerate() {
        if privs & (1 << bit) != 0 {
            out.push(c);
        }
        if goption & (1 << bit) != 0 {
            out.push('*');
        }
    }
    out.push('/');
    out.push_str(&putid(&grantor));
    Ok(out)
}

/// `getid`: skip whitespace, read an identifier -- a run of alphanumerics
/// and `_`, or a double-quoted name with `""` for a quote -- then skip
/// whitespace. Returns the name and the rest of the input.
fn getid(s: &str) -> Result<(String, &str)> {
    let s = s.trim_start();
    let mut name = String::new();
    let mut in_quotes = false;
    let mut chars = s.char_indices().peekable();
    let mut end = s.len();
    while let Some((i, c)) = chars.next() {
        if !(c.is_alphanumeric() || c == '_' || c == '"' || in_quotes) {
            end = i;
            break;
        }
        if c == '"' {
            if chars.peek().map(|(_, n)| *n) != Some('"') {
                in_quotes = !in_quotes;
                continue;
            }
            chars.next();
        }
        name.push(c);
    }
    Ok((name, s[end..].trim_start()))
}

/// `putid`: a name that is not a plain run of alphanumerics and `_` is
/// double-quoted, with `"` doubled. An empty name (PUBLIC) prints as nothing.
fn putid(name: &str) -> String {
    if name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return name.to_string();
    }
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::parse;
    use crate::{set_session_user, take_warnings, Error};

    /// Every line measured on PostgreSQL 16 with `jdrumgoole` as the role.
    #[test]
    fn aclitem_parses_and_renders_as_postgresql() {
        set_session_user(Some("jd".into()));
        take_warnings();
        for (input, want) in [
            ("jd=arwdDxt/jd", "jd=arwdDxt/jd"),
            ("=r/jd", "=r/jd"),
            ("jd=r*w/jd", "jd=r*w/jd"),
            ("jd=wra*/jd", "jd=a*rw/jd"),
            ("jd=*r/jd", "jd=r/jd"),
            ("jd=rr/jd", "jd=r/jd"),
            ("jd=/jd", "jd=/jd"),
            ("\"jd\"=r/\"jd\"", "jd=r/jd"),
            ("group jd=r/jd", "jd=r/jd"),
            ("user jd=r/jd", "jd=r/jd"),
            (" jd =r/ jd ", "jd=r/jd"),
        ] {
            assert_eq!(parse(input).as_deref(), Ok(want), "for {input:?}");
        }
        assert!(take_warnings().is_empty());
        // An omitted grantor defaults to the superuser, with a WARNING --
        // raised even when the rest of the literal then fails.
        assert_eq!(parse("jd=r").as_deref(), Ok("jd=r/jd"));
        assert!(parse("jd=r5/jd").is_err());
        assert_eq!(
            take_warnings(),
            vec![
                (
                    "0L000".to_string(),
                    "defaulting grantor to user ID 10".to_string()
                ),
                (
                    "0L000".to_string(),
                    "defaulting grantor to user ID 10".to_string()
                ),
            ]
        );
        for (input, sqlstate, message) in [
            ("nobody=r/jd", "42704", "role \"nobody\" does not exist"),
            ("jd=r/nobody", "42704", "role \"nobody\" does not exist"),
            ("public=r/jd", "42704", "role \"public\" does not exist"),
            ("jd=q/jd", "22P02", "invalid mode character: must be one of \"arwdDxtXUCTcsA\""),
            (
                "junk",
                "22P02",
                "unrecognized key word: \"junk\"\nHint: ACL key word must be \"group\" or \"user\".",
            ),
            (
                "user",
                "22P02",
                "missing name\nHint: A name must follow the \"group\" or \"user\" key word.",
            ),
            ("jd=r/", "22P02", "a name must follow the \"/\" sign"),
            ("jd=r/jd extra", "22P02", "extra garbage at the end of the ACL specification"),
            (" jd = r / jd ", "22P02", "extra garbage at the end of the ACL specification"),
        ] {
            let err: Error = parse(input).expect_err(input);
            assert_eq!((err.sqlstate(), err.to_string()), (sqlstate, message.to_string()), "for {input:?}");
        }
        take_warnings();
        set_session_user(None);
    }
}
