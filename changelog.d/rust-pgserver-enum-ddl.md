### Types you make yourself

`CREATE TYPE ... AS ENUM` and `DROP TYPE` work on the Rust PostgreSQL server —
which matters more than it sounds, because psycopg's entire enum test file (207
tests) died in one session fixture running exactly that DDL, and nothing behind
it was even measurable.

The enum catalog is written in the Python server's representation, because the
two servers share one store and the doc shapes, collection names and
oid-minting rule are a contract, not an implementation choice: oids are minted
monotonically from a shared counter and never reused (renumbering types would
strand any client that registered a decoder by oid), and a type's array oid is
derived — its own oid plus 100 000 — never stored. An enum created by one
server resolves on the other under the same oid, and the next type minted by
either continues the same sequence.

Enum values work too: `'sad'::mood` validates the label with PostgreSQL's own
error for a miss (`invalid input value for enum mood: "nope"`), the column
carries the enum's minted oid so a client that registered the type decodes it,
`pg_typeof` answers the type, and a case-sensitive name renders quoted through
`regtype` (`"CamelCase"`), exactly as measured. The catalog reads behind
psycopg's `TypeInfo.fetch` see user enums beside the builtins.

`DROP` of an unsupported object kind also stopped leaking Rust debug formatting
into its message — "DROP of Ok(ObjectType)" is now "DROP of a schema" and
friends, naming the kind.

#### Added

- `CREATE TYPE ... AS ENUM`, `DROP TYPE [IF EXISTS]`, enum value casts, enum
  rows in `pg_type` / `to_regtype` / `regtype`, cross-server with shared oids.

#### Fixed

- `DROP` of a non-table leaked a protobuf enum's debug form into the message.
