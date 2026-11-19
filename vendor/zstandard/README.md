# zstd 1.5.5, as mongod builds it

Copied unmodified from mongo **r8.2.11**'s `src/third_party/zstandard`
(commit `3173cdbda3516e78038bceee2fc818a533f36be5`), trimmed to `zstd/lib`
(the library mongo's `BUILD.bazel` compiles) and the licence. mongod's
WiredTiger compresses time-series bucket collections with zstd; SecantusDB
builds WiredTiger's own zstd extension against this copy
(`tasks/mongod-storage-model-plan.md`, Joe's decision of 2026-11-19).

Never edit these files. To refresh, copy the directory whole from a newer
mongo tree and update this note.
