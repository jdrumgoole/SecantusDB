// Cross-driver DDL change-stream smoke — Go.
//
// Opens a change stream on a watched collection, performs
// createIndex + dropIndex, asserts the events come back as
// `createIndexes` / `dropIndexes` operationType strings with the
// expected `operationDescription.indexes` payload. The mongo-go-driver
// is type-strict about the change-event envelope (operationType is a
// known enum on its side), so any wire shape divergence trips here.
package main

import (
	"context"
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

	coll := cli.Database("ddl_xd").Collection("c")
	must(coll.Drop(ctx))
	_, err = coll.InsertOne(ctx, bson.D{{Key: "_id", Value: 1}})
	must(err)

	// Open a change stream, then drive DDL.
	cs, err := coll.Watch(ctx, mongo.Pipeline{},
		options.ChangeStream().SetMaxAwaitTime(2*time.Second))
	must(err)
	defer cs.Close(ctx)

	// Small settle so the cursor's resume token is captured before the writes.
	time.Sleep(300 * time.Millisecond)

	_, err = coll.Indexes().CreateOne(ctx, mongo.IndexModel{
		Keys: bson.D{{Key: "x", Value: 1}},
	})
	must(err)
	must(coll.Indexes().DropOne(ctx, "x_1"))

	// Drain two events.
	got := []string{}
	deadline := time.Now().Add(8 * time.Second)
	for time.Now().Before(deadline) && len(got) < 2 {
		if cs.TryNext(ctx) {
			var e bson.M
			must(cs.Decode(&e))
			got = append(got, e["operationType"].(string))
		} else {
			time.Sleep(200 * time.Millisecond)
		}
	}
	if len(got) != 2 || got[0] != "createIndexes" || got[1] != "dropIndexes" {
		fmt.Fprintf(os.Stderr, "FAIL: got %v, want [createIndexes dropIndexes]\n", got)
		os.Exit(1)
	}
	fmt.Println("OK")
}
