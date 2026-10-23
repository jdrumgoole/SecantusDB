### Rust PostgreSQL server: citext's own functions, and missing types named as written

The citext extension's functions work as in PostgreSQL 15.19. A citext
value keeps its case-insensitive comparison through a VALUES list, a
subquery or a CTE. A missing type in `ALTER TYPE` is named as the statement
wrote it.

#### Fixed

- Fixes part of RP-178
  (`tests/backlog/test_rp_batch39.py::test_scenario[citext's own functions]`
  and `[citext's regexp functions with flags and in FROM]`).
  - **The extension's own functions.** These were 42883 and now compute
    as the extension does: `citext(boolean | char | inet)`, `citext_cmp`,
    `citext_eq` / `_ne` / `_lt` / `_le` / `_gt` / `_ge`, `citext_hash`,
    `citext_hash_extended`, `citext_larger`, `citext_smaller`, the
    `citext_pattern_*` functions, `texticlike`, `texticnlike`,
    `texticregexeq` and `texticregexne`.
  - **Flags.** The regexp functions take flags case-insensitively unless
    the flags hold `c` (`regexp_replace(x::citext, 'b', 'x', 'g')` replaced
    only `x`'s lower-case matches).
  - **In FROM.** `regexp_split_to_table` in FROM is case-insensitive too.
  - **From char(n).** A `char(n)` cast to citext drops its trailing blanks.
- Fixes RP-182 (`tests/backlog/test_rp_batch39.py::test_scenario[a derived citext column compares as citext]`):
  a citext column of a VALUES list, a subquery or a CTE compared, sorted
  and took `min` / `max` as text. `(values ('a'::citext)) v(x)` answered
  `x = 'A'` false, and `min` over `'a'` and `'B'` was `B`.
- Fixes RP-180 (`tests/backlog/test_rp_batch38.py::test_scenario[a missing type named with its schema]`
  and `::test_missing_altered_type_has_no_position`): `ALTER TYPE
  s.nosuch ...` said `type "nosuch" does not exist` and pointed at the
  name. PostgreSQL names it as written (`s.nosuch`, `public.nosuch`) and
  gives no position.

#### Known

- RP-178, what is left: citext's `~<~` operators are 42804, and
  `OPERATOR(s.=)` compares as text while `s` is off the search path
  (`tests/backlog/test_rp_batch38.py::test_scenario[citext's pattern operators and an operator named by its schema]`).
