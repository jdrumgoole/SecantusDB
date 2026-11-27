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
    /// The indent the text started at: a line back at it is one column
    /// short.
    base: usize,
}

impl Out {
    fn new() -> Self {
        Out {
            text: String::new(),
            indent: 0,
            fresh: false,
            base: 0,
        }
    }

    /// Text whose lines start `level` indents in, as a stage at that depth
    /// of a plan prints its expressions.
    fn at(level: usize) -> Self {
        Out {
            text: String::new(),
            indent: level,
            fresh: false,
            base: level,
        }
    }

    fn push(&mut self, s: &str) {
        if self.fresh && self.base > 0 && self.indent == self.base {
            self.text.pop();
        }
        self.text.push_str(s);
        self.fresh = false;
    }

    /// The space after a token, which a new line takes the place of.
    fn space(&mut self) {
        if !self.fresh {
            self.push(" ");
        }
    }

    /// A new line; on a line that is still fresh, that line re-indented
    /// instead, as mongod prints a nested `if` (`else if ...` ends both
    /// blocks on one line; RM-693).
    fn newline(&mut self) {
        let here = self.text.len() - self.text.rfind('\n').map_or(0, |i| i + 1);
        if self.fresh && here != self.indent * 4 {
            let start = self.text.rfind('\n').map_or(0, |i| i + 1);
            self.text.truncate(start);
        } else {
            self.text.push('\n');
        }
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
        Expr::Let { frame, bind, body } => {
            out.indent += 1;
            out.newline();
            out.push("let [");
            out.indent += 1;
            out.newline();
            out.push(&format!("l{frame}.0 = "));
            expr(bind, out);
            out.space();
            out.indent -= 1;
            out.newline();
            out.push("]");
            out.space();
            out.newline();
            out.push("in ");
            expr(body, out);
            out.space();
            out.indent -= 1;
            out.newline();
        }
        Expr::Lambda { frame, body } => {
            out.push(&format!("lambda(l{frame}.0) {{ "));
            expr(body, out);
            out.space();
            out.push("}");
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
        Expr::InList(s, _) => out.push(&format!("s{s}")),
        Expr::ObjSpec { fields, open } => {
            let (shape, ret) = if *open {
                ("Open", "RetInput")
            } else {
                ("Closed", "RetNothing")
            };
            out.push(&format!(
                "MakeObjSpec([{}], {shape}, {ret}, 0)",
                fields.join(", ")
            ));
        }
        Expr::SetSpec(names) => {
            let sets: Vec<String> = names
                .iter()
                .enumerate()
                .map(|(i, n)| format!("{n} = Set({i})"))
                .collect();
            out.push(&format!(
                "MakeObjSpec([{}], Open, NewObj, 0)",
                sets.join(", ")
            ));
        }
    }
}

/// An `InList` as the plan's environment prints it: `InList(1 "b" 2.5)`,
/// its values as the command gave them. `None` for a value this printer
/// is not known to write as mongod does.
pub(super) fn in_list(values: &[Bson]) -> Option<String> {
    let mut parts = Vec::with_capacity(values.len());
    for v in values {
        parts.push(list_value(v)?);
    }
    Some(format!("InList({})", parts.join(" ")))
}

/// One `InList` value (measured 8.2.11: `2.0 1.5 3 -0.0 1e+20 "a b"`,
/// an int64 with no suffix, a double with a point or an exponent).
pub(super) fn list_value(v: &Bson) -> Option<String> {
    Some(match v {
        Bson::Null => "null".to_string(),
        Bson::Boolean(b) => b.to_string(),
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => n.to_string(),
        Bson::Double(d) if d.is_finite() => {
            let text = g17(*d);
            // Written alike at any precision: six significant digits or
            // fewer (what was measured).
            let mantissa = text.split('e').next().unwrap_or(&text);
            let digits = mantissa
                .trim_start_matches('-')
                .replace('.', "")
                .trim_start_matches('0')
                .trim_end_matches('0')
                .len();
            if digits > 6 {
                return None;
            }
            if text.contains(['.', 'e']) {
                text
            } else {
                format!("{text}.0")
            }
        }
        Bson::String(s) if !s.contains(['"', '\\']) && !s.chars().any(char::is_control) => {
            format!("\"{s}\"")
        }
        _ => return None,
    })
}

fn op_text(op: BinOp) -> &'static str {
    match op {
        BinOp::FillEmpty => "?:",
        BinOp::Eq => "==",
        BinOp::Lt => "<",
        BinOp::Lte => "<=",
        BinOp::Gt => ">",
        BinOp::Gte => ">=",
        BinOp::Div => "/",
        BinOp::LogicOr => "||",
        BinOp::LogicAnd => "&&",
    }
}

/// An expression's text, as a stage line prints it at the top.
#[cfg(test)]
pub(super) fn expression(e: &Expr) -> String {
    expression_at(e, 0)
}

/// An expression's text in a stage line `level` indents deep: its lines
/// start there, and a closing line sits one column short of the indent.
fn expression_at(e: &Expr, level: usize) -> String {
    let mut out = Out::at(level);
    expr(e, &mut out);
    if out.fresh && level > 0 {
        out.text.pop();
    }
    out.text
}

/// An expression's text as `executionStats` lists it: with the separator
/// after its last token, unless that ended a line.
pub(super) fn listed(e: &Expr) -> String {
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

/// One line of a stage at `level` indents.
fn push(lines: &mut Vec<String>, level: usize, text: String) {
    lines.push(format!("{}{text}", "    ".repeat(level)));
}

/// One stage's line, then the lines of the stages under it.
fn stage(st: &Stage, level: usize, lines: &mut Vec<String>) {
    match st {
        Stage::IxSeek {
            node,
            record_id,
            low,
            high,
            index,
            uuid,
            fetch,
            ..
        } => {
            let opt = |s: Option<u32>| s.map_or("none".to_string(), |s| format!("s{s}"));
            push(
                lines,
                level,
                format!(
                    "[{node}] ixseek {} {} {} s{record_id} {} {} [] @\"{uuid}\" @\"{index}\" true ",
                    key_string(low),
                    key_string(high),
                    opt(fetch.map(|f| f.0)),
                    opt(fetch.map(|f| f.1)),
                    opt(fetch.map(|f| f.2)),
                ),
            );
        }
        Stage::Seek {
            node,
            seek_record_id,
            record,
            record_id,
            snapshot,
            ident,
            key,
            key_pattern,
            fields,
            uuid,
        } => {
            let fields: Vec<String> = fields.iter().map(|(n, s)| format!("s{s} = {n}")).collect();
            push(lines, level, format!(
                "[{node}] seek s{seek_record_id} s{record} s{record_id} s{snapshot} s{ident} s{key} s{key_pattern} none none [{}] @\"{uuid}\" true false ",
                fields.join(", ")
            ));
        }
        Stage::Scan {
            node,
            record,
            record_id,
            fields,
            uuid,
        } => {
            let fields: Vec<String> = fields.iter().map(|(n, s)| format!("s{s} = {n}")).collect();
            push(lines, level, format!(
                "[{node}] scan s{record} s{record_id} none none none none none none [{}] @\"{uuid}\" true false ",
                fields.join(", ")
            ));
        }
        Stage::Filter {
            node,
            predicate,
            input,
        } => {
            push(
                lines,
                level,
                format!("[{node}] filter {{{}}} ", expression_at(predicate, level)),
            );
            stage(input, level, lines);
        }
        Stage::LimitSkip {
            node,
            limit,
            skip,
            input,
        } => {
            let slot =
                |a: &Option<(u32, i64)>| a.map_or("none".to_string(), |(s, _)| format!("s{s}"));
            match skip {
                None => push(lines, level, format!("[{node}] limit {} ", slot(limit))),
                Some(_) => push(
                    lines,
                    level,
                    format!("[{node}] limitskip {} {} ", slot(limit), slot(skip)),
                ),
            }
            stage(input, level, lines);
        }
        Stage::Project { node, binds, input } => {
            let binds: Vec<String> = binds
                .iter()
                .map(|(s, e)| format!("s{s} = {}", expression_at(e, level)))
                .collect();
            push(
                lines,
                level,
                format!("[{node}] project [{}] ", binds.join(", ")),
            );
            stage(input, level, lines);
        }
        Stage::Sort {
            node,
            keys,
            outputs,
            limit,
            input,
        } => {
            let slots_of: Vec<u32> = keys.iter().map(|(s, _)| *s).collect();
            let dirs: Vec<&str> = keys
                .iter()
                .map(|(_, d)| if *d { "desc" } else { "asc" })
                .collect();
            let mut line = format!(
                "[{node}] sort [{}] [{}] [{}] ",
                slots(&slots_of),
                dirs.join(", "),
                slots(outputs)
            );
            if let Some((s, _)) = limit {
                line.push_str(&format!("s{s} "));
            }
            push(lines, level, line);
            stage(input, level, lines);
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
                .map(|a| format!("s{} = {}", a.slot, expression_at(&a.call(), level)))
                .collect();
            line.push_str(&accs.join(", "));
            line.push_str("] ");
            if !aggs.is_empty() {
                let spills: Vec<u32> = aggs.iter().map(|a| a.spill).collect();
                let merging: Vec<String> = aggs
                    .iter()
                    .map(|a| expression_at(&a.merging, level))
                    .collect();
                line.push_str(&format!(
                    "spillSlots[{}] mergingExprs[{}] ",
                    slots(&spills),
                    merging.join(", ")
                ));
            }
            push(lines, level, line);
            stage(input, level, lines);
        }
        Stage::Coscan { node } => push(lines, level, format!("[{node}] coscan ")),
        Stage::LimitLit { node, limit, input } => {
            push(lines, level, format!("[{node}] limit {limit}ll "));
            stage(input, level, lines);
        }
        Stage::Unwind {
            node,
            input_slot,
            out,
            index,
            preserve,
            input,
        } => {
            push(
                lines,
                level,
                format!("[{node}] unwind s{out} s{index} s{input_slot} {preserve} "),
            );
            stage(input, level, lines);
        }
        Stage::Branch {
            node,
            predicate,
            then_slots,
            else_slots,
            outputs,
            then_,
            else_,
        } => {
            push(
                lines,
                level,
                format!(
                    "[{node}] branch {{{}}} [{}] ",
                    expression_at(predicate, level),
                    slots(outputs)
                ),
            );
            let mark = lines.len();
            stage(then_, level, lines);
            if let Some(first) = lines.get_mut(mark) {
                let (pad, rest) = first.split_at(level * 4);
                *first = format!("{pad}[{}] {rest}", slots(then_slots).replace(", ", " "));
            }
            lines.push(" ".repeat(level * 4));
            let mark = lines.len();
            stage(else_, level, lines);
            if let Some(first) = lines.get_mut(mark) {
                let (pad, rest) = first.split_at(level * 4);
                *first = format!("{pad}[{}] {rest}", slots(else_slots).replace(", ", " "));
            }
        }
        Stage::Union {
            node,
            inputs,
            outputs,
        } => {
            push(
                lines,
                level,
                format!("[{node}] union [{}] ", slots(outputs)),
            );
            for (i, (slots_of, input)) in inputs.iter().enumerate() {
                push(
                    lines,
                    level + 1,
                    format!("branch{i} [{}] ", slots(slots_of)),
                );
                stage(input, level + 2, lines);
            }
        }
        Stage::Nlj {
            node,
            projects,
            correlated,
            outer,
            inner,
        } => {
            push(
                lines,
                level,
                format!(
                    "[{node}] nlj inner [{}] [{}] ",
                    slots(projects),
                    slots(correlated)
                ),
            );
            push(lines, level + 1, "left ".to_string());
            stage(outer, level + 2, lines);
            push(lines, level + 1, "right ".to_string());
            stage(inner, level + 2, lines);
        }
        Stage::HashLookup {
            node,
            outer_key,
            inner_key,
            inner_value,
            out,
            outer,
            inner,
            ..
        } => {
            push(
                lines,
                level,
                format!("[{node}] hash_lookup [s{out} = addToArray(s{inner_value})] "),
            );
            push(lines, level + 1, format!("outer s{outer_key} "));
            stage(outer, level + 2, lines);
            push(
                lines,
                level + 1,
                format!("inner s{inner_key} s{inner_value} "),
            );
            stage(inner, level + 2, lines);
        }
    }
}

/// A seek key as `debugPrint` prints a KeyString: `KS(` and its bytes in
/// upper-case hex.
fn key_string(bytes: &[u8]) -> String {
    let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
    format!("KS({hex})")
}

/// Whether a stage in the tree prints its inputs as indented blocks.
fn has_blocks(st: &Stage) -> bool {
    matches!(
        st,
        Stage::HashLookup { .. } | Stage::Nlj { .. } | Stage::Branch { .. } | Stage::Union { .. }
    ) || st.inputs().into_iter().any(has_blocks)
}

/// `slotBasedPlan.stages`.
pub(super) fn stages(root: &Stage) -> String {
    let mut lines = Vec::new();
    stage(root, 0, &mut lines);
    // A stage with blocks under it (a join, a branch, a union) ends the
    // text with the new line that closes the last block.
    let mut text = lines.join("\n");
    if has_blocks(root) {
        text.push('\n');
    }
    text
}

/// What the scans and seeks under a join read: its `totalDocsExamined`,
/// `totalKeysExamined`, `collectionScans`, `collectionSeeks`,
/// `indexSeeks` and `indexesUsed`.
#[derive(Default)]
struct Tally {
    docs: i64,
    keys: i64,
    scans: i64,
    seeks: i64,
    index_seeks: i64,
    indexes: Vec<String>,
}

fn tally(stage: &Stage, counted: &Counted, t: &mut Tally) {
    match stage {
        Stage::Scan { .. } => {
            t.docs += counted.reads;
            t.scans += 1;
        }
        // A seek of each fetched document (RM-695).
        Stage::Seek { .. } => {
            t.docs += counted.reads;
            t.seeks += counted.reads;
        }
        Stage::IxSeek { index, .. } => {
            t.keys += counted.keys;
            t.index_seeks += 1;
            if !t.indexes.contains(index) {
                t.indexes.push(index.clone());
            }
        }
        _ => {}
    }
    for (input, below) in stage.inputs().into_iter().zip(counted.children()) {
        tally(input, below, t);
    }
}

/// The scan statistics a join stage lists first.
fn join_stats(stage: &Stage, counted: &Counted, out: &mut Document) {
    let mut t = Tally::default();
    tally(stage, counted, &mut t);
    out.insert("totalDocsExamined", super::number(t.docs));
    out.insert("totalKeysExamined", super::number(t.keys));
    out.insert("collectionScans", super::number(t.scans));
    out.insert("collectionSeeks", super::number(t.seeks));
    out.insert("indexScans", 0_i32);
    out.insert("indexSeeks", super::number(t.index_seeks));
    let used: Vec<Bson> = t.indexes.into_iter().map(Bson::String).collect();
    out.insert("indexesUsed", used);
}

/// The fields `executionStats` adds for each kind of stage.
pub(super) fn specific_stats(st: &Stage, counted: &Counted, out: &mut Document) {
    match st {
        Stage::IxSeek {
            record_id,
            low,
            high,
            index,
            fetch,
            ..
        } => {
            out.insert("indexName", index.clone());
            out.insert("keysExamined", super::number(counted.keys));
            out.insert("seeks", super::number(counted.seeks));
            out.insert("numReads", super::number(counted.reads));
            if let Some((key, _, _)) = fetch {
                out.insert("indexKeySlot", super::number(*key as i64));
            }
            out.insert("recordIdSlot", super::number(*record_id as i64));
            if let Some((_, snapshot, ident)) = fetch {
                out.insert("snapshotIdSlot", super::number(*snapshot as i64));
                out.insert("indexIdentSlot", super::number(*ident as i64));
            }
            out.insert("outputSlots", Bson::Array(vec![]));
            // `indexKeysToInclude`: a bitset of the key fields read into
            // slots, none here.
            out.insert("indexKeysToInclude", "0".repeat(32));
            out.insert("seekKeyLow", format!("{} ", key_string(low)));
            out.insert("seekKeyHigh", format!("{} ", key_string(high)));
        }
        Stage::Seek {
            seek_record_id,
            record,
            record_id,
            snapshot,
            ident,
            key,
            key_pattern,
            fields,
            ..
        } => {
            out.insert("numReads", super::number(counted.reads));
            out.insert("recordSlot", super::number(*record as i64));
            out.insert("recordIdSlot", super::number(*record_id as i64));
            out.insert("seekRecordIdSlot", super::number(*seek_record_id as i64));
            out.insert("snapshotIdSlot", super::number(*snapshot as i64));
            out.insert("indexIdentSlot", super::number(*ident as i64));
            out.insert("indexKeySlot", super::number(*key as i64));
            out.insert("indexKeyPatternSlot", super::number(*key_pattern as i64));
            let names: Vec<Bson> = fields
                .iter()
                .map(|(n, _)| Bson::String(n.clone()))
                .collect();
            let slots: Vec<Bson> = fields.iter().map(|(_, s)| Bson::Int64(*s as i64)).collect();
            out.insert("scanFieldNames", names);
            out.insert("scanFieldSlots", slots);
        }
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
        Stage::Filter { predicate, .. } => {
            out.insert("numTested", super::number(counted.tested));
            out.insert("filter", listed(predicate));
        }
        Stage::LimitSkip { limit, skip, .. } => {
            if let Some((_, n)) = limit {
                out.insert("limit", super::number(*n));
            }
            if let Some((_, n)) = skip {
                out.insert("skip", super::number(*n));
            }
        }
        Stage::Project { binds, .. } => {
            let mut p = Document::new();
            for (s, e) in binds {
                p.insert(s.to_string(), listed(e));
            }
            out.insert("projections", p);
        }
        Stage::Sort { keys, outputs, .. } => {
            // `SortStats`: the in-memory sort mongod runs under its default
            // 100 MiB limit, never spilling here.
            out.insert("memLimit", 104_857_600_i32);
            out.insert("totalDataSizeSorted", 0_i32);
            out.insert("usedDisk", false);
            out.insert("spills", 0_i32);
            out.insert("spilledDataStorageSize", 0_i32);
            out.insert("spilledBytes", 0_i32);
            out.insert("spilledRecords", 0_i32);
            let mut order = Document::new();
            for (s, d) in keys {
                order.insert(s.to_string(), if *d { "desc" } else { "asc" });
            }
            out.insert("orderBySlots", order);
            let outs: Vec<Bson> = outputs.iter().map(|s| Bson::Int64(*s as i64)).collect();
            out.insert("outputSlots", outs);
        }
        Stage::Coscan { .. } => {}
        Stage::LimitLit { limit, .. } => {
            // A limit that was never opened reports no amount.
            if counted.opens != Some(0) {
                out.insert("limit", super::number(*limit));
            }
        }
        Stage::Unwind {
            input_slot,
            out: out_slot,
            index,
            preserve,
            ..
        } => {
            out.insert("inputSlot", super::number(*input_slot as i64));
            out.insert("outSlot", super::number(*out_slot as i64));
            out.insert("outIndexSlot", super::number(*index as i64));
            out.insert("preserveNullAndEmptyArrays", i32::from(*preserve));
        }
        Stage::Branch {
            predicate,
            then_slots,
            else_slots,
            outputs,
            ..
        } => {
            let list =
                |v: &[u32]| -> Vec<Bson> { v.iter().map(|s| Bson::Int64(*s as i64)).collect() };
            out.insert("numTested", super::number(counted.tested));
            out.insert("thenBranchOpens", super::number(counted.then_opens));
            let closes = |i: usize| counted.kids.get(i).and_then(|k| k.closes).unwrap_or(0);
            out.insert("thenBranchCloses", super::number(closes(0)));
            out.insert("elseBranchOpens", super::number(counted.else_opens));
            out.insert("elseBranchCloses", super::number(closes(1)));
            out.insert("filter", listed(predicate));
            out.insert("thenSlots", list(then_slots));
            out.insert("elseSlots", list(else_slots));
            out.insert("outputSlots", list(outputs));
        }
        Stage::Union {
            inputs, outputs, ..
        } => {
            let list =
                |v: Vec<u32>| -> Vec<Bson> { v.iter().map(|s| Bson::Int64(*s as i64)).collect() };
            out.insert(
                "inputSlots",
                list(inputs.iter().flat_map(|(s, _)| s.iter().copied()).collect()),
            );
            out.insert("outputSlots", list(outputs.clone()));
        }
        Stage::Nlj {
            projects,
            correlated,
            ..
        } => {
            let list =
                |v: &[u32]| -> Vec<Bson> { v.iter().map(|s| Bson::Int64(*s as i64)).collect() };
            join_stats(st, counted, out);
            out.insert("innerOpens", super::number(counted.inner_opens));
            out.insert("innerCloses", super::number(counted.inner_closes));
            out.insert("outerProjects", list(projects));
            out.insert("outerCorrelated", list(correlated));
        }
        Stage::HashLookup { .. } => {
            join_stats(st, counted, out);
            out.insert("usedDisk", false);
            out.insert("spills", 0_i32);
            out.insert("spilledRecords", 0_i32);
            out.insert("spilledBytes", 0_i32);
            out.insert("spilledDataStorageSize", 0_i32);
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
