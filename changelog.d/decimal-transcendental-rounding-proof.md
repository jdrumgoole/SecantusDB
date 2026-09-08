### Decimal128 transcendentals: one of the three options is now ruled out

The previous write-up costed three options for the finite-operand transcendentals
and flagged "add a pure-Rust decimal crate" as the risky one. A further
measurement rules it out entirely.

Computing at 60 digits and rounding correctly to 34 matches mongod for `$sqrt`
and `$exp` — and **does not** for `$ln` or `$log10`:

```
ln(2.5)    = 0.9162907318741550651835272117680110|714501…
             correctly rounded → …680111      mongod → …680110   (below)

log10(2.5) = 0.3979400086720376095725222105510139|464636…
             correctly rounded → …510139      mongod → …510140   (above)
```

mongod lands *below* the true value for one and *above* it for the other, so it
is neither correctly rounded nor consistently truncated — it carries Intel RDFP's
own approximation error. Matching it means reproducing that error, which a more
accurate library cannot do at any precision.

So the choice is now binary: link Intel RDFP (bit-identical, at the cost of a C
dependency across five wheel platforms), or keep refusing (today's behaviour).
The backlog entry records the evidence.
