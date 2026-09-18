### php-library's text-index failure reads as the declared gap it is

`IndexInfoFunctionalTest::testIsText` fails with the server's own
`text indexes are not supported by SecantusDB`. Text indexes are permanently
out of scope, and the same gap was already declared for the node and pymongo
gauges — php-library was the one left reading as an unexplained failure, which
is what made its report look like it had something to chase.

#### Changed

- `docs/validation-report-php-lib.md` now carries **Expected** and **Adjusted**
  columns, and an "Expected failures" section naming the test and why it fails:
  3089 passed / 0 failed / 1 expected — **99.9% plain, 100.0% adjusted**.

  Both columns ship. The plain rate still counts the failure, so it stays
  visible; the adjusted one answers "how much of the conformable surface
  conforms". Declaring a gap is not the same as hiding it, and the gauge is
  never told to skip the test.
