//! The weighted operation mix a load worker draws from.

use crate::BenchResult;

/// The operations the load agent knows how to issue.
pub const OP_NAMES: [&str; 3] = ["insert", "find", "update"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Insert,
    Find,
    Update,
}

impl Op {
    pub fn name(self) -> &'static str {
        match self {
            Op::Insert => "insert",
            Op::Find => "find",
            Op::Update => "update",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Op::Insert => 0,
            Op::Find => 1,
            Op::Update => 2,
        }
    }

    fn parse(name: &str) -> Option<Op> {
        match name {
            "insert" => Some(Op::Insert),
            "find" => Some(Op::Find),
            "update" => Some(Op::Update),
            _ => None,
        }
    }
}

/// `"insert=70,find=20,update=10"` -> cumulative bounds in `[0, 1]`.
///
/// Zero-weight ops are dropped rather than left as unreachable entries, and the
/// final bound is forced to exactly 1.0 so float drift can never leave a sliver
/// of probability that falls off the end of the table.
pub fn parse_op_mix(spec: &str) -> BenchResult<Vec<(Op, f64)>> {
    let mut weights = [0.0f64; 3];
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (name, raw) = part
            .split_once('=')
            .ok_or_else(|| format!("bad --op-mix term {part:?}: expected name=weight"))?;
        let op = Op::parse(name.trim()).ok_or_else(|| {
            format!(
                "unknown op {:?} in --op-mix (known: {})",
                name.trim(),
                OP_NAMES.join(", ")
            )
        })?;
        let value: f64 = raw
            .trim()
            .parse()
            .map_err(|_| format!("bad weight {:?} for op {:?}", raw.trim(), name.trim()))?;
        weights[op.index()] = value;
    }
    let total: f64 = weights.iter().sum();
    if total <= 0.0 {
        return Err("--op-mix must contain at least one positive weight".to_string());
    }
    let mut out = Vec::new();
    let mut acc = 0.0;
    for (idx, op) in [Op::Insert, Op::Find, Op::Update].into_iter().enumerate() {
        if weights[idx] <= 0.0 {
            continue;
        }
        acc += weights[idx] / total;
        out.push((op, acc));
    }
    let last = out.len() - 1;
    out[last].1 = 1.0;
    Ok(out)
}

/// Pick the op whose cumulative bound first covers `roll` (a uniform [0, 1)).
pub fn pick(mix: &[(Op, f64)], roll: f64) -> Op {
    for (op, bound) in mix {
        if roll <= *bound {
            return *op;
        }
    }
    mix[mix.len() - 1].0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_are_normalised_and_ordered() {
        let mix = parse_op_mix("insert=70,find=20,update=10").unwrap();
        assert_eq!(
            mix.iter().map(|(o, _)| o.name()).collect::<Vec<_>>(),
            ["insert", "find", "update"]
        );
        assert!((mix[0].1 - 0.7).abs() < 1e-9);
        assert!((mix[1].1 - 0.9).abs() < 1e-9);
        assert_eq!(mix[2].1, 1.0);
    }

    #[test]
    fn last_bound_is_exactly_one() {
        // Float drift below 1.0 would silently skew the last op's share.
        let mix = parse_op_mix("insert=1,find=1,update=1").unwrap();
        assert_eq!(mix.last().unwrap().1, 1.0);
    }

    #[test]
    fn zero_weight_ops_are_dropped() {
        let mix = parse_op_mix("insert=100,find=0").unwrap();
        assert_eq!(mix.len(), 1);
        assert_eq!(mix[0].0, Op::Insert);
    }

    #[test]
    fn bad_specs_are_rejected() {
        for spec in ["nonsense=1", "insert", "insert=zero", "insert=0"] {
            assert!(parse_op_mix(spec).is_err(), "{spec} should be rejected");
        }
    }

    #[test]
    fn pick_respects_the_bounds() {
        let mix = parse_op_mix("insert=70,find=20,update=10").unwrap();
        assert_eq!(pick(&mix, 0.0), Op::Insert);
        assert_eq!(pick(&mix, 0.69), Op::Insert);
        assert_eq!(pick(&mix, 0.75), Op::Find);
        assert_eq!(pick(&mix, 0.95), Op::Update);
        assert_eq!(pick(&mix, 1.0), Op::Update);
    }
}
