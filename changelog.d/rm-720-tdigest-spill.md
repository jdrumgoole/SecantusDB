### A spilled `$median` / `$percentile` keeps mongod's t-digest

A classic `$group`'s approximate `$median` and `$percentile` now keep
mongod 8.2's t-digest as the values arrive, instead of every value until
finalize. Its memory is mongod's `TDigest::memUsageBytes` (the buffer and
the centroids each flush leaves), its spilled state is the digest's merged
centroids (`TDigestDistributedClassic::serialize`), and spilled states are
merged back as digests (`combine`), so a group spills where mongod's does
and answers what mongod's answers.

#### Fixed

- One group of 30000 values under a 70000-byte `$group` budget spilled
  never and answered 5552.178172875341; it now spills 5 times and answers
  5551.9832, as mongod 8.2.11. Three interleaved groups that spill answer
  mongod's medians and percentiles (they were off in the second decimal).

Fixes RM-720 (tests/backlog/test_rm_720_spilled_median.py::test_a_median_counts_its_centroids_and_spills_them, tests/backlog/test_rm_720_spilled_median.py::test_a_spilled_median_is_mongods_merged_tdigest)
