//! mongod's SBE debug printer (`exec/sbe/util/debug_print.cpp`): the text
//! of `slotBasedPlan.stages` and of the expressions `executionStats` lists.
//!
//! The printer lays out a stream of tokens. Each token is followed by a
//! space, except where the printer asks for none; `if` blocks start on a
//! new line, indented one level (four spaces) deeper than the line they
//! belong to, and close with a new line back at that line's indent.

use super::expr::{BinOp, Expr};
use super::stage::{Counted, Stage};
use bson::{Bson, Document};

/// Text being laid out, and the indent of the line it is on.
struct Out {
    text: String,
    indent: usize,
    /// Nothing written since the last new line: a separator is dropped.
    fresh: bool,
}

impl Out {
    fn new() -> Self {
        Out {
            text: String::new(),
            indent: 0,
            fresh: false,
        }
    }

    fn push(&mut self, s: &str) {
        self.text.push_str(s);
        self.fresh = false;
    }

    /// The space after a token, which a new line takes the place of.
    fn space(&mut self) {
        if !self.fresh {
            self.push(" ");
        }
    }

    fn newline(&mut self) {
        self.text.push('\n');
        for _ in 0..self.indent {
            self.text.push_str("    ");
        }
        self.fresh = true;
    }
}

/// A constant as the printer writes it.
fn constant(v: &Option<Bson>) -> String {
    match v {
        None => "Nothing".to_string(),
        Some(Bson::Null) => "null".to_string(),
        Some(Bson::Boolean(b)) => b.to_string(),
        Some(Bson::Int32(n)) => n.to_string(),
        Some(Bson::Int64(n)) => format!("{n}ll"),
        Some(Bson::Double(d)) => format!("{}L", g17(*d)),
        // Written as it is, with no escaping (`value_printer.cpp`).
        Some(Bson::String(s)) => format!("\"{s}\""),
        Some(other) => other.to_string(),
    }
}

/// A double as a `std::stringstream` at `max_digits10` (17) precision
/// writes it: C's `%.17g`.
fn g17(d: f64) -> String {
    if d.is_nan() {
        return "nan".into();
    }
    if d.is_infinite() {
        return if d > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if d == 0.0 {
        return if d.is_sign_negative() {
            "-0".into()
        } else {
            "0".into()
        };
    }
    let sci = format!("{d:.16e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let trim = |t: &str| -> String {
        if t.contains('.') {
            t.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            t.to_string()
        }
    };
    if !(-4..17).contains(&exp) {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim(mantissa), exp.abs())
    } else {
        let decimals = (16 - exp).max(0) as usize;
        trim(&format!("{d:.decimals$}"))
    }
}

fn expr(e: &Expr, out: &mut Out) {
    match e {
        Expr::Slot(s) => out.push(&format!("s{s}")),
        Expr::Local { frame, moved } => {
            if *moved {
                out.push(&format!("move(l{frame}.0)"));
            } else {
                out.push(&format!("l{frame}.0"));
            }
        }
        Expr::Const(v) => out.push(&constant(v)),
        Expr::Int64(n) => out.push(&format!("{n}ll")),
        Expr::Func(name, args) => {
            out.push(name);
            out.push("(");
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push(", ");
                }
                expr(a, out);
            }
            out.push(")");
        }
        Expr::Bin(op, l, r) => {
            out.push("(");
            expr(l, out);
            out.space();
            out.push(op_text(*op));
            out.push(" ");
            expr(r, out);
            out.push(")");
        }
        Expr::Not(e) => {
            out.push("!(");
            expr(e, out);
            out.push(")");
        }
        Expr::If(c, t, f) => {
            out.indent += 1;
            out.newline();
            out.push("if ");
            expr(c, out);
            out.space();
            out.newline();
            out.push("then ");
            expr(t, out);
            out.space();
            out.newline();
            out.push("else ");
            expr(f, out);
            out.space();
            out.indent -= 1;
            out.newline();
        }
        Expr::Lambda { frame, body } => {
            out.push(&format!("lambda(l{frame}.0) {{ "));
            expr(body, out);
            out.push(" }");
        }
        Expr::TypeMatch(e, mask) => {
            out.push("typeMatch(");
            expr(e, out);
            out.push(&format!(", {mask})"));
        }
        Expr::ConvertInt32(e) => {
            out.push("convert ( ");
            expr(e, out);
            out.push(", int32)");
        }
    }
}

fn op_text(op: BinOp) -> &'static str {
    match op {
        BinOp::FillEmpty => "?:",
        BinOp::Eq => "==",
        BinOp::Div => "/",
        BinOp::LogicOr => "||",
    }
}

/// An expression's text, as a stage line prints it.
pub(super) fn expression(e: &Expr) -> String {
    let mut out = Out::new();
    expr(e, &mut out);
    out.text
}

/// An expression's text as `executionStats` lists it: with the separator
/// after its last token, unless that ended a line.
fn listed(e: &Expr) -> String {
    let mut out = Out::new();
    expr(e, &mut out);
    out.space();
    out.text
}

fn slots(list: &[u32]) -> String {
    list.iter()
        .map(|s| format!("s{s}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// One stage's line, then the lines of the stages under it.
fn stage(st: &Stage, lines: &mut Vec<String>) {
    match st {
        Stage::Scan {
            node,
            record,
            record_id,
            fields,
            uuid,
        } => {
            let fields: Vec<String> = fields.iter().map(|(n, s)| format!("s{s} = {n}")).collect();
            lines.push(format!(
                "[{node}] scan s{record} s{record_id} none none none none none none [{}] @\"{uuid}\" true false ",
                fields.join(", ")
            ));
        }
        Stage::Project { node, binds, input } => {
            let binds: Vec<String> = binds
                .iter()
                .map(|(s, e)| format!("s{s} = {}", expression(e)))
                .collect();
            lines.push(format!("[{node}] project [{}] ", binds.join(", ")));
            stage(input, lines);
        }
        Stage::Group {
            node,
            keys,
            aggs,
            input,
        } => {
            let mut line = format!("[{node}] group [{}] [", slots(keys));
            let accs: Vec<String> = aggs
                .iter()
                .map(|a| format!("s{} = {}", a.slot, expression(&a.call())))
                .collect();
            line.push_str(&accs.join(", "));
            line.push_str("] ");
            if !aggs.is_empty() {
                let spills: Vec<u32> = aggs.iter().map(|a| a.spill).collect();
                let merging: Vec<String> = aggs.iter().map(|a| expression(&a.merging)).collect();
                line.push_str(&format!(
                    "spillSlots[{}] mergingExprs[{}] ",
                    slots(&spills),
                    merging.join(", ")
                ));
            }
            lines.push(line);
            stage(input, lines);
        }
    }
}

/// `slotBasedPlan.stages`.
pub(super) fn stages(root: &Stage) -> String {
    let mut lines = Vec::new();
    stage(root, &mut lines);
    lines.join("\n")
}

/// The fields `executionStats` adds for each kind of stage.
pub(super) fn specific_stats(st: &Stage, counted: &Counted, out: &mut Document) {
    match st {
        Stage::Scan {
            record,
            record_id,
            fields,
            ..
        } => {
            out.insert("numReads", super::number(counted.reads));
            out.insert("recordSlot", super::number(*record as i64));
            out.insert("recordIdSlot", super::number(*record_id as i64));
            let names: Vec<Bson> = fields
                .iter()
                .map(|(n, _)| Bson::String(n.clone()))
                .collect();
            let slots: Vec<Bson> = fields.iter().map(|(_, s)| Bson::Int64(*s as i64)).collect();
            out.insert("scanFieldNames", names);
            out.insert("scanFieldSlots", slots);
        }
        Stage::Project { binds, .. } => {
            let mut p = Document::new();
            for (s, e) in binds {
                p.insert(s.to_string(), listed(e));
            }
            out.insert("projections", p);
        }
        Stage::Group { keys, aggs, .. } => {
            let keys: Vec<Bson> = keys.iter().map(|s| Bson::Int64(*s as i64)).collect();
            out.insert("groupBySlots", keys);
            if !aggs.is_empty() {
                let mut exprs = Document::new();
                let mut init = Document::new();
                let mut merging = Document::new();
                for a in aggs {
                    exprs.insert(a.slot.to_string(), listed(&a.call()));
                    init.insert(a.slot.to_string(), Bson::Null);
                    merging.insert(a.spill.to_string(), listed(&a.merging));
                }
                exprs.insert("initExprs", init);
                out.insert("expressions", exprs);
                out.insert("mergingExprs", merging);
            }
            out.insert("usedDisk", false);
            out.insert("spills", 0_i32);
            out.insert("spilledBytes", 0_i32);
            out.insert("spilledRecords", 0_i32);
            out.insert("spilledDataStorageSize", 0_i32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::g17;

    /// What mongod 8.2.11 prints for a double constant, less its `L`.
    #[test]
    fn doubles_print_at_17_digits() {
        for (d, text) in [
            (1.5, "1.5"),
            (1.0, "1"),
            (0.1, "0.10000000000000001"),
            (1e20, "1e+20"),
            (2.0, "2"),
            (-0.5, "-0.5"),
            (1e-5, "1.0000000000000001e-05"),
            (123456.0, "123456"),
        ] {
            assert_eq!(g17(d), text, "{d}");
        }
    }
}
