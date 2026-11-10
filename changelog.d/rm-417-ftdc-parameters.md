### FTDC sampling follows its parameters, and samples the transport layer

The Rust MongoDB server's diagnostic sample, which `getDiagnosticData`
answers, now follows mongod's two FTDC switches: `setParameter
diagnosticDataCollectionPeriodMillis` changes how often it samples (from the
next boundary of the new period, at least 100 ms), and
`diagnosticDataCollectionEnabled: false` stops sampling while
`getDiagnosticData` keeps answering the last sample. `getParameter` reports
both. Each sample also carries mongod's `transportLayerStats` collector,
naming the server's TCP listener.

#### Fixed

- `getParameter` / `setParameter` know `diagnosticDataCollectionEnabled`
  and `diagnosticDataCollectionPeriodMillis`, coerced and refused as mongod
  8.2.11 does, and the FTDC collector honours them.
- The FTDC sample has a `transportLayerStats` collector after
  `serverStatus`.

Fixes part of RM-417 (tests/backlog/test_rm_219_diag_query_settings.py::test_ftdc_parameters_and_transport_collector,
tests/backlog/test_rm_219_diag_query_settings.py::test_transport_layer_stats_name_the_tcp_listener,
tests/backlog/test_rm_219_diag_query_settings.py::test_the_period_and_the_switch_take_effect);
writing samples to `diagnostic.data` stays open. Files RM-541.
