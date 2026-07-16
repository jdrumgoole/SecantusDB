### Docs: the site banner tops every self-hosted docs page

Both self-hosted docs trees (secantusdb.com/docs/ and /docs/rust/) now carry
the standard site banner — SecantusDB · Python DB · Rust DB · Blog ·
Python docs · Rust docs — via furo's announcement bar, so the documentation
reads as part of secantusdb.com rather than a detached sub-site. The
readthedocs.io copies keep their "docs have moved" banner instead (the two
are the same announcement slot, switched on the READTHEDOCS build env var).
