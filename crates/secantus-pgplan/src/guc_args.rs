//! A `SET` / `ALTER SYSTEM SET` value list as the one text PostgreSQL
//! stores: `flatten_set_variable_args`. Only a parameter that takes a list
//! (`GUC_LIST_INPUT`) may be given more than one value; the values are
//! joined by `, `, and a list of identifiers (`GUC_LIST_QUOTE`) has each
//! string quoted as `quote_identifier` quotes it, so `set search_path = 'a,
//! b'` names one schema, `"a, b"`. Numbers are kept as written.

use super::*;

/// The parameters that take a list (`GUC_LIST_INPUT`), measured on
/// PostgreSQL 15.19: every other one refuses a second value.
const LIST_INPUT: [&str; 13] = [
    "datestyle",
    "listen_addresses",
    "local_preload_libraries",
    "log_destination",
    "output_plugin_libraries",
    "restrict_nonsystem_relation_kind",
    "search_path",
    "session_preload_libraries",
    "shared_preload_libraries",
    "synchronous_standby_names",
    "temp_tablespaces",
    "unix_socket_directories",
    "wal_consistency_checking",
];

/// The lists whose items are identifiers (`GUC_LIST_QUOTE`).
const LIST_QUOTE: [&str; 7] = [
    "local_preload_libraries",
    "output_plugin_libraries",
    "search_path",
    "session_preload_libraries",
    "shared_preload_libraries",
    "temp_tablespaces",
    "unix_socket_directories",
];

/// `args` flattened for parameter `name` (as written).
pub(crate) fn flatten(name: &str, args: &[pg_query::protobuf::Node]) -> Result<String> {
    let lower = name.to_ascii_lowercase();
    if args.len() > 1 && !LIST_INPUT.contains(&lower.as_str()) {
        return Err(Error::Sqlstate(
            "22023",
            format!("SET {name} takes only one argument"),
        ));
    }
    let quote = LIST_QUOTE.contains(&lower.as_str());
    let mut parts = Vec::with_capacity(args.len());
    for a in args {
        if let Some(N::AConst(c)) = a.node.as_ref() {
            match c.val.as_ref() {
                // A numeric literal is kept as written (`0.6`, not a
                // decimal's form).
                Some(pg_query::protobuf::a_const::Val::Fval(f)) => {
                    parts.push(f.fval.clone());
                    continue;
                }
                Some(pg_query::protobuf::a_const::Val::Ival(i)) => {
                    parts.push(i.ival.to_string());
                    continue;
                }
                Some(pg_query::protobuf::a_const::Val::Sval(s)) => {
                    parts.push(if quote {
                        scalar::quote_identifier(&s.sval)
                    } else {
                        s.sval.clone()
                    });
                    continue;
                }
                _ => {}
            }
        }
        // `SET TIME ZONE INTERVAL '...' HOUR TO MINUTE`: the value of the
        // cast.
        parts.push(match const_value(a, &[])? {
            Bson::String(s) => s,
            Bson::Int32(i) => i.to_string(),
            Bson::Int64(i) => i.to_string(),
            Bson::Double(d) => d.to_string(),
            Bson::Boolean(b) => (if b { "on" } else { "off" }).to_string(),
            Bson::Null => String::new(),
            other => value_text(&other),
        });
    }
    Ok(parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(sql: &str) -> Result<String> {
        let N::VariableSetStmt(v) = parse_one(sql).expect("parses") else {
            panic!("not a SET");
        };
        flatten(&v.name, &v.args)
    }

    #[test]
    fn lists_quote_their_identifiers_as_postgres_does() {
        // Measured on 15.19.
        assert_eq!(flat("set search_path = 'a, b'").unwrap(), "\"a, b\"");
        assert_eq!(
            flat("set search_path = public, \"$user\", \"Foo\", 'x y', bar").unwrap(),
            "public, \"$user\", \"Foo\", \"x y\", bar"
        );
        assert_eq!(flat("set search_path = ''").unwrap(), "\"\"");
        assert_eq!(flat("set search_path = 1.5").unwrap(), "1.5");
        assert_eq!(flat("set datestyle = 'iso', 'dmy'").unwrap(), "iso, dmy");
    }

    #[test]
    fn only_a_list_takes_two_values() {
        let err = flat("set work_mem = 1, 2").unwrap_err();
        assert!(
            matches!(err, Error::Sqlstate("22023", m) if m == "SET work_mem takes only one argument")
        );
        assert!(flat("set application_name = 'a', 'b'").is_err());
    }
}
