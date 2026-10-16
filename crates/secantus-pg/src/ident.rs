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
    parse_in(&text, map, path.parent()).map_err(|(line, message)| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "user name map file {}, line {line}: {message}",
                path.display()
            ),
        )
    })
}

#[cfg(test)]
fn parse(text: &str, map: &str) -> Result<UserMap, (usize, String)> {
    parse_in(text, map, None)
}

/// The map `map` of a `pg_ident.conf`'s text, an `@file` read relative to
/// `dir` (the file's own directory).
fn parse_in(text: &str, map: &str, dir: Option<&Path>) -> Result<UserMap, (usize, String)> {
    let mut entries = Vec::new();
    for (number, line) in logical_lines(text) {
        let fields = fields(&line, dir, 0).map_err(|message| (number, message))?;
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
            // Read as the SQL `~` operator reads a pattern, POSIX classes
            // and all, so a map and a query agree on what one matches.
            Some(pattern) => SystemName::Pattern(
                Regex::new(&secantus_pgplan::scalar::pg_regex_source(pattern)).map_err(|e| {
                    (
                        number,
                        format!("invalid regular expression \"{pattern}\": {e}"),
                    )
                })?,
            ),
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
fn fields(line: &str, dir: Option<&Path>, depth: usize) -> Result<Vec<String>, String> {
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
        // `@file` (unquoted): the field is the one value that file holds
        // (`parse_ident_line` refuses more than one).
        if !was_quoted && token.len() > 1 && token.starts_with('@') {
            let mut values = included(&token[1..], dir, depth)?;
            if values.len() != 1 {
                return Err("multiple values in ident field".into());
            }
            out.push(values.remove(0));
            continue;
        }
        out.push(token);
    }
}

/// Every value in an `@file`, comments dropped.
fn included(name: &str, dir: Option<&Path>, depth: usize) -> Result<Vec<String>, String> {
    if depth >= 10 {
        return Err(format!(
            "could not open file \"{name}\": too many levels of inclusion"
        ));
    }
    let path = match dir {
        Some(dir) if Path::new(name).is_relative() => dir.join(name),
        _ => Path::new(name).to_path_buf(),
    };
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "could not open secondary authentication file \"@{name}\" as \"{}\": {e}",
            path.display()
        )
    })?;
    let mut out = Vec::new();
    for (_, line) in logical_lines(&text) {
        out.extend(fields(&line, path.parent(), depth + 1)?);
    }
    Ok(out)
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
letters \"/^([[:alpha:]]+)$\"   \\1
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
    fn an_at_file_is_the_one_value_it_holds() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("who"), "laptop  # a comment\n").unwrap();
        std::fs::write(dir.path().join("two"), "a\nb\n").unwrap();
        let map = parse_in("m @who alice\n", "m", Some(dir.path())).expect("parses");
        assert!(map.allows("laptop", "alice"));
        let err = parse_in("m @two alice\n", "m", Some(dir.path())).expect_err("two values");
        assert!(err.1.contains("multiple values in ident field"), "{err:?}");
    }

    #[test]
    fn a_pattern_reads_posix_classes_as_sql_does() {
        // `[[:alpha:]]` counts `é` as a letter, as PostgreSQL's does under a
        // UTF-8 locale and as the SQL `~` operator here does.
        let letters = map("letters");
        assert!(letters.allows("émile", "émile"));
        assert!(!letters.allows("emile2", "emile2"));
    }

    #[test]
    fn quotes_keep_commas_and_a_backslash_joins_lines() {
        assert!(map("exact").allows("CN=alice,OU=Eng,O=Ex\\, ample,C=IE", "alice"));
        assert!(map("long").allows("a_very_long_name", "mapped"));
        assert_eq!(
            fields("a \"b \"\"c\"\" d\" e", None, 0).unwrap(),
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
            (
                "m @absent-names-file a\n",
                1,
                "could not open secondary authentication file",
            ),
        ] {
            let (at, message) = parse(text, "m").expect_err(text);
            assert_eq!(at, line, "{text}");
            assert!(message.contains(part), "{text}: {message}");
        }
    }
}
