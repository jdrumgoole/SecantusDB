# snappy 1.1.10, as mongod builds it

Copied unmodified from mongo **r8.2.11**'s `src/third_party/snappy`
(commit `3173cdbda3516e78038bceee2fc818a533f36be5`), trimmed to the files
mongo's `BUILD.bazel` compiles, its per-platform `config.h` headers, and the
licence. mongod's WiredTiger compresses its catalog, collections and journal
with snappy; SecantusDB builds WiredTiger's own snappy extension against
this copy so its stores are the ones mongod writes
(`tasks/mongod-storage-model-plan.md`, Joe's decision of 2026-11-19).

Never edit these files. To refresh, copy the directory whole from a newer
mongo tree and update this note.
