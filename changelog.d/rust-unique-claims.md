### Unique indexes hold across a transaction

A unique index on the Rust server could be persuaded to accept two documents
with the same value. If one writer was inside a transaction and another was
not, each checked for a clash by reading its own snapshot of the data — and
neither snapshot showed the other's pending write. Both were told they were
fine, both were written, and the index that was supposed to guarantee
uniqueness quietly held a duplicate. Nothing failed, nothing was logged; the
damage only became visible later, in the data.

The clash check no longer relies on reading. Each unique value is now claimed
in a table keyed by the value itself, so the storage engine refuses the second
claim outright, whoever makes it and whenever they started. A writer that
arrives while a transaction holds the value now waits for it, exactly as
MongoDB does, and is then told the value is taken — or, if the transaction was
rolled back, quietly takes it.

Claims are released when the row that owns them is deleted, and cleared when
the collection, database or index they belong to is dropped, so a value can
always be used again once nothing is using it.

#### Fixed

- A unique index no longer accepts a duplicate when one of the writers is
  inside a transaction.
