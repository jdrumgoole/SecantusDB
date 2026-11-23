### A `$facet` element that is not a stage is refused by name

The Rust MongoDB server now refuses a `$facet` pipeline element that is not an object as mongod 8.2.11 does: 40171, naming the element's type, index and value, and before the generic pipeline-element check.

#### Fixed

- `[{$facet: {b: [5]}}]` answered 14 "Each element of the 'pipeline' array must be an object" (and a direct 40171 said only "contained an invalid element"); it is now 40171 "... b argument contained an element of type int: 0: 5". An empty object element still passes to the pipeline's 40323, as on mongod.

Fixes RM-647 (tests/backlog/test_rm_found_2026_11_14_rm530.py::test_a_facet_element_refusal_names_the_element)
