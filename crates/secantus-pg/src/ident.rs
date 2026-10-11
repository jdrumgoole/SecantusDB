//! A user name map: which database roles the holder of a client certificate
//! may log in as. The file is PostgreSQL's `pg_ident.conf`, read as
//! PostgreSQL 15 reads it:
//!
//! ```text
//! # map-name  system-username        database-username
//! staff       alice_laptop           alice
//! staff       "/^(.*)@example\.com$" \1
//! ```
//!
//! The system user name is what the certificate says: its common name, or
//! its whole subject under `clientname=DN`. A system user name that begins
//! with `/` is a regular expression, and `\1` in the database user name is
//! what its first group matched.
//!
//! The regular expressions are the `regex` crate's, not PostgreSQL's
//! advanced regular expressions. The two agree on what a name map is
//! written with; one this crate cannot compile is an error naming its line.

use std::io;
use std::path::Path;

use regex::Regex;

enum SystemName {
    Exact(String),
    Pattern(Regex),
}

struct Entry {
    system: SystemName,
    role: String,
}

/// The lines of one named map.
pub(crate) struct UserMap {
    entries: Vec<Entry>,
}

impl std::fmt::Debug for UserMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UserMap({} entries)", self.entries.len())
    }
}

impl UserMap {
    /// May the holder of `system` log in as `role`? Both compare exactly:
    /// PostgreSQL folds case only for the methods that ask it to, and `cert`
    /// is not one.
    pub(crate) fn allows(&self, system: &str, role: &str) -> bool {
        self.entries.iter().any(|entry| match &entry.system {
            SystemName::Exact(name) => name == system && entry.role == role,
            SystemName::Pattern(pattern) => {
                let Some(found) = pattern.captures(system) else {
                    return false;
                };
                match entry.role.find("\\1") {
                    // Only the first `\1` is replaced, as PostgreSQL has it.
                    Some(at) => found.get(1).is_some_and(|group| {
                        let mut mapped = entry.role[..at].to_string();
                        mapped.push_str(group.as_str());
                        mapped.push_str(&entry.role[at + 2..]);
                        mapped == role
                    }),
                    None => entry.role == role,
                }
            }
        })
    }
}

/// Read the map called `map` out of the file at `path`. Every line of the
/// file is checked, as PostgreSQL refuses a file with any bad line; a map
/// name no line carries is a map that lets nobody in, which is also
/// PostgreSQL's reading.
pub(crate) fn read(path: &Path, map: &str) -> io::Result<UserMap> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("user name map file {}: {e}", path.display()),
        )
    })?;
    parse(&text, map).map_err(|(line, message)| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "user name map file {}, line {line}: {message}",
                path.display()
            ),
        )
    })
}

fn parse(text: &str, map: &str) -> Result<UserMap, (usize, String)> {
    let mut entries = Vec::new();
    for (number, line) in logical_lines(text) {
        let fields = fields(&line).map_err(|message| (number, message))?;
        if fields.is_empty() {
            continue;
        }
        let [name, system, role] = fields.as_slice() else {
            return Err((
                number,
                match fields.len() {
                    1 => "missing entry at end of line: no system user name".to_string(),
                    2 => "missing entry at end of line: no database user name".to_string(),
                    _ => "more than three fields".to_string(),
                },
            ));
        };
        let system = match system.strip_prefix('/') {
            Some(pattern) => SystemName::Pattern(Regex::new(pattern).map_err(|e| {
                (
                    number,
                    format!("invalid regular expression \"{pattern}\": {e}"),
                )
            })?),
            None => SystemName::Exact(system.clone()),
        };
        if let SystemName::Pattern(pattern) = &system {
            if role.contains("\\1") && pattern.captures_len() < 2 {
                return Err((
                    number,
                    format!(
                        "regular expression \"{}\" has no subexpressions as requested by \
                         backreference in \"{role}\"",
                        pattern.as_str()
                    ),
                ));
            }
        }
        if name == map {
            entries.push(Entry {
                system,
                role: role.clone(),
            });
        }
    }
    Ok(UserMap { entries })
}

/// The file's lines, a line ending in a backslash joined to the next, each
/// with the number of its first physical line.
fn logical_lines(text: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut open: Option<(usize, String)> = None;
    for (index, line) in text.lines().enumerate() {
        let (number, mut whole) = open.take().unwrap_or((index + 1, String::new()));
        match line.strip_suffix('\\') {
            Some(head) => {
                whole.push_str(head);
                open = Some((number, whole));
            }
            None => {
                whole.push_str(line);
                out.push((number, whole));
            }
        }
    }
    out.extend(open);
    out
}

/// One line's fields. A field is one token; tokens are separated by blanks,
/// a `#` outside quotes starts a comment, and double quotes keep blanks,
/// commas and `#` in a token (`""` inside them is one quote). An unquoted
/// comma makes a list, which no field of this file may be.
fn fields(line: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        match chars.peek() {
            None | Some('#') => return Ok(out),
            Some(_) => {}
        }
        let mut token = String::new();
        let mut quoted = false;
        let mut was_quoted = false;
        while let Some(&c) = chars.peek() {
            if !quoted && (c.is_whitespace() || c == '#') {
                break;
            }
            chars.next();
            match c {
                '"' if quoted && chars.peek() == Some(&'"') => {
                    chars.next();
                    token.push('"');
                }
                '"' => {
                    quoted = !quoted;
                    was_quoted = true;
                }
                ',' if !quoted => {
                    return Err(
                        "multiple values in a field; quote a value that holds a comma".into(),
                    )
                }
                _ => token.push(c),
            }
        }
        if quoted {
            return Err("unterminated quoted string".into());
        }
        if !was_quoted && token.starts_with('@') {
            return Err(format!("file inclusion ({token}) is not supported"));
        }
        out.push(token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "\
# a comment
staff   alice_laptop   alice     # and another
staff   shared         alice
staff   shared         postgres
staff   \"/^(.*)@example\\.com$\"   \\1
other   bobcert        postgres
dn      \"/^CN=(.*),O=Example$\"  \\1
exact   \"CN=alice,OU=Eng,O=Ex\\, ample,C=IE\"  alice
fixed   /^svc-            service
long    a_very_long_name \\
        mapped
";

    fn map(name: &str) -> UserMap {
        parse(FILE, name).expect("the file parses")
    }

    #[test]
    fn an_exact_line_maps_one_name_to_one_role() {
        let staff = map("staff");
        assert!(staff.allows("alice_laptop", "alice"));
        assert!(!staff.allows("alice_laptop", "postgres"));
        assert!(!staff.allows("alice", "alice"), "no line names alice");
        assert!(!staff.allows("Alice_Laptop", "alice"), "case matters");
        assert!(staff.allows("shared", "alice") && staff.allows("shared", "postgres"));
        assert!(!staff.allows("bobcert", "postgres"), "another map's line");
        assert!(map("other").allows("bobcert", "postgres"));
    }

    #[test]
    fn a_pattern_maps_what_its_group_matched() {
        let staff = map("staff");
        assert!(staff.allows("carol@example.com", "carol"));
        assert!(!staff.allows("carol@example.com", "alice"));
        assert!(!staff.allows("carol@example.org", "carol"));
        let dn = map("dn");
        assert!(dn.allows("CN=alice,O=Example", "alice"));
        assert!(!dn.allows("CN=alice", "alice"));
        // A pattern with no `\1` maps everything it matches to one role.
        let fixed = map("fixed");
        assert!(fixed.allows("svc-backup", "service"));
        assert!(!fixed.allows("backup", "service"));
    }

    #[test]
    fn quotes_keep_commas_and_a_backslash_joins_lines() {
        assert!(map("exact").allows("CN=alice,OU=Eng,O=Ex\\, ample,C=IE", "alice"));
        assert!(map("long").allows("a_very_long_name", "mapped"));
        assert_eq!(
            fields("a \"b \"\"c\"\" d\" e").unwrap(),
            ["a", "b \"c\" d", "e"]
        );
    }

    #[test]
    fn a_map_no_line_names_lets_nobody_in() {
        assert!(!map("absent").allows("alice", "alice"));
    }

    #[test]
    fn a_bad_line_is_an_error_with_its_number() {
        for (text, line, part) in [
            ("m a\n", 1, "no database user name"),
            ("m\n", 1, "no system user name"),
            ("\nm a b c\n", 2, "more than three"),
            ("m /^CN=(.*),O=x$ \\1\n", 1, "multiple values"),
            ("m \"/(\" a\n", 1, "invalid regular expression"),
            ("m /^a$ \\1\n", 1, "no subexpressions"),
            ("m \"a b\n", 1, "unterminated"),
            ("m @names a\n", 1, "not supported"),
        ] {
            let (at, message) = parse(text, "m").expect_err(text);
            assert_eq!(at, line, "{text}");
            assert!(message.contains(part), "{text}: {message}");
        }
    }
}
