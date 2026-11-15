### Rust MongoDB server: a tailable `find` of a collection that is not capped prints its query

mongod refuses the read with the query it was asked, as its planner prints it
(`ns=<db>.pTree: $and\nSort: {}\nProj: {}\n tailable cursor requested on
non capped collection`); this server's message now does too.

#### Fixed

- Fixes RM-498 (tests/backlog/test_rm_found_2026_11_10_rm363.py::test_a_tailable_find_of_a_collection_that_is_not_capped_prints_its_query)
