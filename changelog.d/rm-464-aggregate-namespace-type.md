### An `aggregate` naming a collection of the wrong type is InvalidNamespace

mongod reads an `aggregate` command's namespace after the field pass. A string names a collection. A number that equals 1 is the collectionless form, and any other number is BadValue (2) "Invalid command format: the 'aggregate' field must specify a collection name or 1". Any other type is InvalidNamespace (73) "Collection name has invalid type <type>". The Rust MongoDB server answered BadValue for every non-string type: `true`, `null`, `{}`, an array, an ObjectId and so on. It also truncated a fractional number, so `aggregate: 1.5` ran as a collectionless aggregate where mongod 8.2.11 refuses it.

#### Fixed

- `secantus-commands` (`aggregate`): a namespace that is neither a string nor a number is 73 "Collection name has invalid type <type>", naming the type as mongod 8.2.11 does. A number is the collectionless form only when its value equals 1 (`1.0` and Decimal128 `1.0` do; `1.5`, Decimal128 `1.5` and NaN are BadValue).
- Fixes RM-464 (tests/backlog/test_rm_aggregate_parse.py::test_aggregate_names_a_mistyped_namespace). The test now covers every BSON type, the fractional and non-1 numbers, and the order of the namespace check against `pipeline` and `cursor`.
