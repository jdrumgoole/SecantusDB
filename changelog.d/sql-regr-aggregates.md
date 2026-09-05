### The two-argument statistical aggregates

#### Added

- `corr`, `covar_pop`, `covar_samp`, and `regr_avgx`, `regr_avgy`,
  `regr_count`, `regr_intercept`, `regr_r2`, `regr_slope`, `regr_sxx`,
  `regr_sxy`, `regr_syy` — all previously `0A000`.

They are one feature rather than twelve: every one is derived from the same six
sums, so they share a single accumulator set and a single post-aggregate, and
differ only in the finishing arithmetic. A pair contributes only when **both**
arguments are non-null, which is what makes `regr_count` disagree with
`count(*)` and what keeps the means over the same population the count reports.

`regr_count` returns `int8` and is the only one defined over an empty input (0,
where the others are NULL); the rest return `float8`.
