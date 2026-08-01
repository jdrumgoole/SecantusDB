// The whole driver: walk the (include-filtered) corpus with cockroach's own
// pgtest runner against a running SecantusPGServer. Corpus dir, server
// address, and user arrive via environment (set by pgtest_validation.runner).
package pgtestgauge

import (
	"os"
	"testing"

	"github.com/cockroachdb/cockroach/pkg/testutils/pgtest"
)

func TestPGTest(t *testing.T) {
	dir := os.Getenv("PGTEST_DATADIR")
	addr := os.Getenv("PGTEST_ADDR")
	user := os.Getenv("PGTEST_USER")
	if dir == "" || addr == "" {
		t.Fatal("PGTEST_DATADIR and PGTEST_ADDR must be set")
	}
	pgtest.WalkWithRunningServer(t, dir, addr, user)
}
