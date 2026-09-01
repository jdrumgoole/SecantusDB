### Collated sorting now orders the way MongoDB orders

Collation was implemented for *matching* and only for matching. Each string was
folded down to a single value, and anything the fold did not distinguish fell
back to comparing whole codepoints — so a collated sort put every accented word
after `z` instead of beside its base letter, had no tertiary case order at all,
and accepted `caseFirst`, `backwards` and `numericOrdering` while ignoring them.
Sorting `["a", "á", "ä", "az", "b"]` under `{locale: "en"}` gave `a az b á ä`
where MongoDB gives `a á ä az b`.

Sorting is a different problem from matching, and it now has its own key: three
levels in the shape ICU uses. The primary level is the base letters with accents
removed and case folded (or split into digit runs when `numericOrdering` is on,
so `a2` sorts before `a10`). The secondary level is the accents, one entry per
base character, weighted by an order measured against MongoDB rather than by
codepoint — acute sorts before grave even though the codepoints run the other
way — and reversed when `backwards` is set, which is what makes French `cote <
côte < coté`. The tertiary level is case, flipped by `caseFirst`. `strength`
truncates the key and `caseLevel` re-adds the case rank.

Measuring it turned up something worse than wrong order: a collated sort
returned a **different** order depending on whether a collated index existed,
because the index's byte order is the single-level normalisation and could never
express the levels. The index is still used to fetch — `explain` still reports
IXSCAN and the scan stays narrow — but it no longer counts as having satisfied
the sort. An index must change speed, never results.

Seventeen of nineteen cases in the new sweep now match MongoDB 8.2.11 exactly.
The two that do not are locale-specific: Swedish sorts `ä` after `z` and Danish
sorts `å` last, which is CLDR data rather than something decomposition can
derive, and needs an ICU dependency.

#### Fixed

- `collation.py`: `sort_levels` builds the three-level ordering key; `Collation`
  gained `caseFirst` and `backwards`.
- `ordering.py`: `sort_docs` takes a collation and uses it for string
  comparison.
- `storage.py`: `find_matching` sorts with the query's collation, and no longer
  treats a collated index walk as already sorted.
- `aggregate.py`: the `$sort` stage sorts with the pipeline's collation.

#### Added

- `tools/probes/collation_order.py`: the standing sweep, run with and without a
  collated index so the two can never diverge again.
