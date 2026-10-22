//! Named and defaulted arguments to a built-in function (RP-074):
//! `make_interval(days => 1)` is the positional call PostgreSQL resolves it
//! to, every argument it leaves out filled from the function's declared
//! default (`ParseFuncOrColumn` / `expand_function_arguments`).
//!
//! `pg_proc_argnames.tsv` is PostgreSQL 15.19's `pg_proc`: for each built-in
//! with every input argument named, its input types, their names and each
//! one's default (`pg_get_function_arg_default`, `-` for none). A user
//! function is the server's own business and is left alone.

use super::*;
use std::collections::HashMap;
use std::sync::OnceLock;

const ARGNAMES: &str = include_str!("pg_proc_argnames.tsv");

struct Signature {
    names: Vec<String>,
    defaults: Vec<Option<String>>,
}

fn signatures() -> &'static HashMap<String, Vec<Signature>> {
    static S: OnceLock<HashMap<String, Vec<Signature>>> = OnceLock::new();
    S.get_or_init(|| {
        let mut m: HashMap<String, Vec<Signature>> = HashMap::new();
        for line in ARGNAMES.lines() {
            let mut it = line.split('\t');
            let (Some(name), Some(_types), Some(names), Some(defaults)) =
                (it.next(), it.next(), it.next(), it.next())
            else {
                continue;
            };
            m.entry(name.to_string()).or_default().push(Signature {
                names: names.split(',').map(str::to_string).collect(),
                defaults: defaults
                    .split('|')
                    .map(|d| (d != "-").then(|| d.to_string()))
                    .collect(),
            });
        }
        m
    })
}

fn named(a: &pg_query::protobuf::Node) -> Option<&pg_query::protobuf::NamedArgExpr> {
    match a.node.as_ref() {
        Some(N::NamedArgExpr(na)) => Some(na),
        _ => None,
    }
}

/// Is this a call to a built-in with a named argument?
pub(crate) fn wanted(f: &pg_query::protobuf::FuncCall) -> bool {
    f.args.iter().any(|a| named(a).is_some())
        && func_name(f)
            .is_some_and(|n| signatures().contains_key(&n) && !correlated::user_function_named(&n))
}

/// The overload a call names by its arguments' NAMES: every named argument
/// one of its parameters past the positional ones, each once, and every
/// parameter not given one with a default.
fn choose<'a>(sigs: &'a [Signature], f: &pg_query::protobuf::FuncCall) -> Option<&'a Signature> {
    let positional = f.args.iter().take_while(|a| named(a).is_none()).count();
    let given: Vec<&str> = f
        .args
        .iter()
        .filter_map(named)
        .map(|n| n.name.as_str())
        .collect();
    let fits = |s: &Signature| {
        if positional > s.names.len() {
            return false;
        }
        let mut filled = vec![false; s.names.len()];
        filled.iter_mut().take(positional).for_each(|f| *f = true);
        for g in &given {
            match s.names.iter().position(|n| n == g) {
                Some(i) if !filled[i] => filled[i] = true,
                _ => return false,
            }
        }
        filled
            .iter()
            .zip(&s.defaults)
            .all(|(f, d)| *f || d.is_some())
    };
    let mut fitting = sigs.iter().filter(|s| fits(s));
    let first = fitting.next()?;
    // Two overloads that both fit by name are told apart by the types,
    // which is the ordinary resolution's to do: left alone then.
    fitting.next().is_none().then_some(first)
}

/// The call with its arguments in position, defaults filled in.
pub(crate) fn replacement(n: &pg_query::protobuf::Node) -> Option<pg_query::protobuf::Node> {
    let Some(N::FuncCall(f)) = n.node.as_ref() else {
        return None;
    };
    if !wanted(f) {
        return None;
    }
    let name = func_name(f)?;
    let sig = choose(signatures().get(&name)?, f)?;
    let mut args: Vec<Option<pg_query::protobuf::Node>> = vec![None; sig.names.len()];
    for (i, a) in f.args.iter().enumerate() {
        match named(a) {
            Some(na) => {
                let slot = sig.names.iter().position(|n| *n == na.name)?;
                args[slot] = na.arg.as_deref().cloned();
            }
            None => args[i] = Some(a.clone()),
        }
    }
    // Every default written in, so the call is complete in position.
    let mut positional = Vec::with_capacity(args.len());
    for (i, a) in args.into_iter().enumerate() {
        positional.push(match a {
            Some(a) => a,
            None => default_node(sig.defaults[i].as_deref()?)?,
        });
    }
    let mut call = (**f).clone();
    call.args = positional;
    Some(pg_query::protobuf::Node {
        node: Some(N::FuncCall(Box::new(call))),
    })
}

fn default_node(sql: &str) -> Option<pg_query::protobuf::Node> {
    let Ok(N::SelectStmt(s)) = parse_one(&format!("SELECT {sql}")) else {
        return None;
    };
    match s.target_list.into_iter().next()?.node? {
        N::ResTarget(rt) => rt.val.map(|v| *v),
        _ => None,
    }
}
