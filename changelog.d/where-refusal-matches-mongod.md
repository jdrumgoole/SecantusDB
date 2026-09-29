### `$where` is refused the way a mongod without a script engine refuses it

`$where` runs user-supplied JavaScript, and SecantusDB embeds no script engine.
Both servers already declined it, but each invented its own answer: the Rust
server said `BadValue: query uses a construct the Rust server does not support`
— our implementation leaking onto the wire — and the Python server said
`unknown top level operator: $where`, which is wrong twice over, because mongod
knows `$where` perfectly well.

`mongod --noscripting` is a supported configuration with exactly the property we
have, and it answers `6108304 / Location6108304 / "no globalScriptEngine in
$where parsing"`. Both servers now answer that, on every command that takes a
filter, with the per-statement `writeErrors` shape on the two batch writes that
mongod uses. Inside an aggregation `$match` the answer is different and
deliberately so — `2 / BadValue / "$where is not allowed in this context"`,
which mongod gives whether or not it has a script engine, so it is a pipeline
rule rather than a scripting one.

This does not make `$where` work: a query that needs the JavaScript to run still
fails, exactly as it does against a real `--noscripting` mongod. What it fixes is
that a client reading the error now learns something true.

#### Fixed
- `$where` in a query context answers mongod's `6108304 Location6108304 "no globalScriptEngine in $where parsing"` on both servers, for `find`, `count`, `distinct`, `findAndModify`, and per-statement in `writeErrors` for `delete` / `update`, at parse time so an empty or nonexistent collection refuses too.
- `$where` inside an aggregation `$match` answers `2 BadValue "$where is not allowed in this context"`, and is checked before the leading-`$match` lift — previously the first stage picked up the query-context refusal while every other position answered correctly.
- `command_error_during` / `read_exec_error` sent the bare sentinel `codeName: "Location"` for any error code with no symbolic name, where mongod sends `Location<n>`. Affected every such code reaching a non-batch command through the storage error path, not just `$where`.

#### Added
- `mongod_noscripting_uri` fixture in the differential gate, so a refusal that only a script-engine-less mongod can demonstrate is compared against a live one rather than against hardcoded values.
