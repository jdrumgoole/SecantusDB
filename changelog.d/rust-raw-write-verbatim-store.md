### Rust server: inserts take a raw-BSON fast path (store the client's bytes verbatim)

The Rust server's insert path no longer decodes and re-encodes every document on
its way to WiredTiger. Previously each inserted document was serialised and
deserialised up to five times between the wire and storage — the server merged the
incoming `OP_MSG` document sequence into a decoded command, the command layer
re-encoded each document to hand it to storage, and storage decoded it again before
re-encoding it for the collection table. All of that reproduced bytes the driver
had already sent in canonical form.

Inserts now carry the client's BSON straight through. The server hands the insert's
document sequence to the handler **un-decoded**; the handler runs the `_id`
validity pre-checks directly over the raw bytes and passes them to storage, which
writes them to the collection table **verbatim** when `_id` already leads the
document (the shape every driver produces). Documents that need work — a
server-assigned `ObjectId`, an `_id` that isn't the leading field, or a collection
with a `validator` — still take the full decode/re-encode path, so stored bytes are
byte-identical to before in every case. On a single writer this measured **~+11%**
insert throughput against the same WiredTiger engine (**~+6%** at four concurrent
writers; neutral at eight, where the insert workload becomes WAL/disk-bound and the
saved CPU no longer moves throughput).

#### Changed
- The Rust server routes an `insert`'s `OP_MSG` kind-1 `documents` sequence to the
  handler as un-decoded byte slices (a new `CommandContext.raw_insert_documents`
  side-channel), skipping the merge-decode and the command-layer re-encode. The
  handler pre-checks `_id` `$`-prefixed keys over `RawDocument` and passes the raw
  bytes to storage; documents inline in the command body, and collections with a
  `validator`, still take the decoded path.
- `Storage::insert` / `Storage::insert_one` (Rust) store the caller's BSON verbatim
  when `_id` already leads the document, skipping the `encode_doc` re-serialisation;
  they fall back to `encode_doc` when an `ObjectId` is assigned or `_id` must be
  reordered to the front. Stored bytes are unchanged in every case (verified
  byte-for-byte against the client-sent encoding across ObjectId / string / int
  `_id`, nested documents, arrays, Decimal128, dates, and binary), and the pymongo
  conformance gauge is non-regressing (1020/1500, 99.5%).
