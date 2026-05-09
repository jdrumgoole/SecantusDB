// Cross-driver postBatchResumeToken smoke — Go.
//
// Open a change stream on a quiet collection (no writes) and pull
// the cursor's resume position via two getMores. Since no events
// arrive, the driver's `cursor.ResumeToken()` should still advance
// across getMores — proves the server returns a non-stale
// `postBatchResumeToken` even on empty batches. The token is
// opaque to the driver but its hex bytes must differ between the
// two polls.
package main

import (
	"context"
	"encoding/hex"
	"fmt"
	"os"
	"time"

	"go.mongodb.org/mongo-driver/v2/bson"
	"go.mongodb.org/mongo-driver/v2/mongo"
	"go.mongodb.org/mongo-driver/v2/mongo/options"
)

func must(err error) {
	if err != nil {
		fmt.Fprintf(os.Stderr, "fatal: %v\n", err)
		os.Exit(1)
	}
}

func tokenHex(token bson.Raw) string {
	if token == nil {
		return ""
	}
	return hex.EncodeToString(token)
}

func main() {
	uri := os.Getenv("MONGODB_URI")
	if uri == "" {
		fmt.Fprintln(os.Stderr, "MONGODB_URI required")
		os.Exit(2)
	}
	ctx := context.Background()
	cli, err := mongo.Connect(options.Client().ApplyURI(uri))
	must(err)
	defer cli.Disconnect(ctx)

	coll := cli.Database("pbrt_xd").Collection("c")
	must(coll.Drop(ctx))

	cs, err := coll.Watch(ctx, mongo.Pipeline{},
		options.ChangeStream().SetMaxAwaitTime(500*time.Millisecond))
	must(err)
	defer cs.Close(ctx)

	// Drain — empty getMore. Sleep + try a few times to let
	// cluster-time advance between polls.
	first := tokenHex(cs.ResumeToken())
	cs.TryNext(ctx)
	time.Sleep(200 * time.Millisecond)
	cs.TryNext(ctx)
	time.Sleep(200 * time.Millisecond)
	cs.TryNext(ctx)
	second := tokenHex(cs.ResumeToken())

	if second == "" {
		fmt.Fprintf(os.Stderr, "FAIL: no resume token after empty getMores (initial=%q)\n", first)
		os.Exit(1)
	}
	if first != "" && first == second {
		fmt.Fprintf(os.Stderr,
			"FAIL: resume token did not advance across empty getMores: %s\n",
			first)
		os.Exit(1)
	}
	fmt.Println("OK")
}
