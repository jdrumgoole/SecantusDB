### The RP-303 regression test widens its own window

#### Fixed

- `tests/backlog/test_rp_named_parse_cancel.py` aims a cancel at the gap
  between a named and an unnamed Parse of the same statement. With its
  200,000-item list that gap was 0.65 s on a local debug build but 0.19 s
  on a CI runner (PR #2395), under the 0.2 s the test needs to aim. The test
  now doubles the list, up to 800,000 items, until the gap is over 0.3 s, and
  reports every size it measured when none is wide enough. The assertions
  about the cancel itself are unchanged.
