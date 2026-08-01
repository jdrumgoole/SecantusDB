// Package skip is a minimal stand-in for cockroach's internal
// pkg/testutils/skip — just enough surface for the verbatim
// pkg/testutils/pgtest runner (fetched at gauge time) to compile.
package skip

import "testing"

// IgnoreLint skips the current test.
func IgnoreLint(t testing.TB, args ...interface{}) {
	t.Helper()
	t.Skip(args...)
}
