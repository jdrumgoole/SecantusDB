### The Rust MongoDB server writes its FTDC samples to `diagnostic.data`

Every full-time diagnostic data capture sample is now written to
`<dbpath>/diagnostic.data` in mongod's own archive format, so mongod's FTDC
decoders read it: `metrics.<UTC time>-00000` files that open with the
`buildInfo` / `getCmdLineOpts` / `hostInfo` metadata, then zlib-compressed
metric chunks of a reference sample and its delta-encoded successors, with
`metrics.interim` carrying the chunk in progress into the next start.

#### Fixed

- `diagnosticDataCollectionDirectoryPath` answers the directory, and a change
  to it is refused 220 `FTDCPathAlreadySet`, as on mongod 8.2.11.
- `diagnosticDataCollectionDirectorySizeMB`, `...FileSizeMB`,
  `...SamplesPerChunk` and `...SamplesPerInterimUpdate` are registered with
  mongod's minimums and its file-versus-directory size check; files rotate at
  the file size and the oldest are removed past the directory size.

Fixes RM-417 (tests/backlog/test_rm_219_diag_query_settings.py::test_samples_are_written_to_diagnostic_data)

#### Filed

- RM-690: the archive writes no `type: 2` periodic metadata.
