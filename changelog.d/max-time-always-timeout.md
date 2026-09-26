### `maxTimeAlwaysTimeOut` failpoint

Both servers now honour mongod's `maxTimeAlwaysTimeOut` failpoint, which driver
test suites use to provoke `MaxTimeMSExpired` deterministically.

#### Added

- Both servers: with `configureFailPoint: "maxTimeAlwaysTimeOut"` on
  (`alwaysOn` or `{times: N}`), any operation that has a `maxTimeMS` fails with
  `50 MaxTimeMSExpired` before doing its work; operations without one run
  normally. A `getMore` counts as time-limited when its cursor was opened with
  `maxTimeMS`, because mongod bounds a non-tailable cursor's getMores by the
  originating command's limit and drivers do not resend it. The failpoint was
  previously accepted and ignored, so four pymongo tests failed with
  "ExecutionTimeout not raised".
