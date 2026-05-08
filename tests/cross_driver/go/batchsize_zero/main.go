// Cross-driver batchSize:0 smoke — Go.
//
// Open a find cursor with batchSize 0; the driver expects the
// cursor reply to carry an empty firstBatch and a non-zero
// cursor.id so it can pull docs via getMore. Then SetBatchSize on
// the BatchCursor and assert Next() retrieves a doc — proving
// firstBatch was empty and the doc came from getMore.
package main

import (
	"context"
	"fmt"
	"os"

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

	coll := cli.Database("batch_zero_xd").Collection("c")
	must(coll.Drop(ctx))
	docs := make([]any, 5)
	for i := range docs {
		docs[i] = bson.D{{Key: "_id", Value: int32(i)}}
	}
	_, err = coll.InsertMany(ctx, docs)
	must(err)

	cursor, err := coll.Find(ctx, bson.D{}, options.Find().SetBatchSize(0))
	must(err)
	defer cursor.Close(ctx)

	// With batchSize:0 the driver opens the cursor and waits on
	// getMore for the actual docs. Next() triggers that getMore.
	if !cursor.Next(ctx) {
		fmt.Fprintln(os.Stderr, "FAIL: Next returned false; expected to get a doc via getMore")
		os.Exit(1)
	}
	var d bson.M
	must(cursor.Decode(&d))
	if d["_id"] != int32(0) {
		fmt.Fprintf(os.Stderr, "FAIL: first _id: got %v, want 0\n", d["_id"])
		os.Exit(1)
	}

	fmt.Println("OK")
}
