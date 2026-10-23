//! `CREATE / ALTER / DROP TEXT SEARCH CONFIGURATION`: a configuration and
//! its map, each token type of its parser to the dictionaries it is offered
//! to (`pg_ts_config`, `pg_ts_config_map`). The executor resolves the names
//! and keeps the rows; `ts_catalog` serves them to `to_tsvector` and the
//! rest.

use super::*;

/// A configuration's name as written: its parts, schema first when
/// qualified.
pub type QualifiedName = Vec<String>;

/// What `CREATE TEXT SEARCH CONFIGURATION` starts from.
#[derive(Debug, Clone, PartialEq)]
pub enum TsSource {
    /// `PARSER = p`: an empty map.
    Parser(QualifiedName),
    /// `COPY = c`: c's parser and map.
    Copy(QualifiedName),
}

/// One `ALTER TEXT SEARCH CONFIGURATION ... MAPPING` action.
#[derive(Debug, Clone, PartialEq)]
pub enum MapAction {
    /// `ADD MAPPING FOR types WITH dicts`.
    Add {
        tokens: Vec<String>,
        dicts: Vec<QualifiedName>,
    },
    /// `ALTER MAPPING FOR types WITH dicts`: the types' dictionaries replaced.
    AlterFor {
        tokens: Vec<String>,
        dicts: Vec<QualifiedName>,
    },
    /// `ALTER MAPPING [FOR types] REPLACE old WITH new`.
    Replace {
        tokens: Vec<String>,
        old: QualifiedName,
        new: QualifiedName,
    },
    /// `DROP MAPPING [IF EXISTS] FOR types`.
    Drop {
        tokens: Vec<String>,
        missing_ok: bool,
    },
}

/// A text-search configuration statement.
#[derive(Debug, Clone, PartialEq)]
pub enum TsConfigOp {
    Create {
        name: QualifiedName,
        source: TsSource,
    },
    Alter {
        name: QualifiedName,
        action: MapAction,
    },
    Drop {
        names: Vec<QualifiedName>,
        if_exists: bool,
        cascade: bool,
    },
    Rename {
        name: QualifiedName,
        to: String,
    },
    Owner {
        name: QualifiedName,
        owner: String,
    },
    SetSchema {
        name: QualifiedName,
        schema: String,
    },
}

/// The strings of a name node: a `List` of them, a `TypeName`'s, or one.
fn parts(n: &pg_query::protobuf::Node) -> QualifiedName {
    match n.node.as_ref() {
        Some(N::String(s)) => vec![s.sval.clone()],
        Some(N::List(l)) => l.items.iter().flat_map(parts).collect(),
        Some(N::TypeName(t)) => t.names.iter().flat_map(parts).collect(),
        _ => Vec::new(),
    }
}

/// `CREATE TEXT SEARCH CONFIGURATION name (PARSER = p | COPY = c)`.
pub(crate) fn plan_create(d: &pg_query::protobuf::DefineStmt) -> Result<Statement> {
    let name: QualifiedName = d.defnames.iter().flat_map(parts).collect();
    let (mut parser, mut copy) = (None, None);
    for e in &d.definition {
        let Some(N::DefElem(e)) = e.node.as_ref() else {
            continue;
        };
        let value = e.arg.as_deref().map(parts).unwrap_or_default();
        match e.defname.to_ascii_lowercase().as_str() {
            "parser" => parser = Some(value),
            "copy" => copy = Some(value),
            other => {
                return Err(Error::Sqlstate(
                    "42601",
                    format!("text search configuration parameter \"{other}\" not recognized"),
                ))
            }
        }
    }
    let source = match (parser, copy) {
        (Some(_), Some(_)) => {
            return Err(Error::Sqlstate(
                "42601",
                "cannot specify both PARSER and COPY options".into(),
            ))
        }
        (Some(p), None) => TsSource::Parser(p),
        (None, Some(c)) => TsSource::Copy(c),
        (None, None) => {
            return Err(Error::Sqlstate(
                "42P17",
                "text search parser is required".into(),
            ))
        }
    };
    Ok(Statement::TsConfig(TsConfigOp::Create { name, source }))
}

/// `ALTER TEXT SEARCH CONFIGURATION name ... MAPPING ...`.
pub(crate) fn plan_alter(a: &pg_query::protobuf::AlterTsConfigurationStmt) -> Result<Statement> {
    use pg_query::protobuf::AlterTsConfigType as K;
    let name: QualifiedName = a.cfgname.iter().flat_map(parts).collect();
    let tokens: Vec<String> = a
        .tokentype
        .iter()
        .flat_map(parts)
        .map(|t| t.to_ascii_lowercase())
        .collect();
    let dicts: Vec<QualifiedName> = a.dicts.iter().map(parts).collect();
    let action = match K::try_from(a.kind) {
        Ok(K::AlterTsconfigAddMapping) => MapAction::Add { tokens, dicts },
        Ok(K::AlterTsconfigAlterMappingForToken) => MapAction::AlterFor { tokens, dicts },
        Ok(K::AlterTsconfigReplaceDict | K::AlterTsconfigReplaceDictForToken) => {
            let [old, new] = dicts.as_slice() else {
                return Err(Error::Parse(
                    "ALTER MAPPING REPLACE takes two dictionaries".into(),
                ));
            };
            MapAction::Replace {
                tokens,
                old: old.clone(),
                new: new.clone(),
            }
        }
        Ok(K::AlterTsconfigDropMapping) => MapAction::Drop {
            tokens,
            missing_ok: a.missing_ok,
        },
        _ => {
            return Err(Error::Unsupported(
                "this ALTER TEXT SEARCH CONFIGURATION form".into(),
            ))
        }
    };
    Ok(Statement::TsConfig(TsConfigOp::Alter { name, action }))
}

/// `DROP TEXT SEARCH CONFIGURATION [IF EXISTS] names [CASCADE]`.
pub(crate) fn plan_drop(d: &pg_query::protobuf::DropStmt) -> Result<Statement> {
    Ok(Statement::TsConfig(TsConfigOp::Drop {
        names: d.objects.iter().map(parts).collect(),
        if_exists: d.missing_ok,
        cascade: pg_query::protobuf::DropBehavior::try_from(d.behavior)
            == Ok(pg_query::protobuf::DropBehavior::DropCascade),
    }))
}

/// `ALTER TEXT SEARCH CONFIGURATION name RENAME TO new`.
pub(crate) fn plan_rename(r: &pg_query::protobuf::RenameStmt) -> Result<Statement> {
    Ok(Statement::TsConfig(TsConfigOp::Rename {
        name: r.object.as_deref().map(parts).unwrap_or_default(),
        to: r.newname.clone(),
    }))
}

/// `ALTER TEXT SEARCH CONFIGURATION name OWNER TO role`.
pub(crate) fn plan_owner(a: &pg_query::protobuf::AlterOwnerStmt) -> Result<Statement> {
    Ok(Statement::TsConfig(TsConfigOp::Owner {
        name: a.object.as_deref().map(parts).unwrap_or_default(),
        owner: a.newowner.as_ref().map(role_spec_name).unwrap_or_default(),
    }))
}

/// `ALTER TEXT SEARCH CONFIGURATION name SET SCHEMA s`.
pub(crate) fn plan_set_schema(a: &pg_query::protobuf::AlterObjectSchemaStmt) -> Result<Statement> {
    Ok(Statement::TsConfig(TsConfigOp::SetSchema {
        name: a.object.as_deref().map(parts).unwrap_or_default(),
        schema: a.newschema.clone(),
    }))
}
