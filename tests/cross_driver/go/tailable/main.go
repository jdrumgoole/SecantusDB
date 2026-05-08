// Cross-driver tailable cursor smoke — Go.
//
// Create a capped collection, seed one doc, open a tailable
// cursor, drain the doc, then insert another doc and verify the
// tailable cursor surfaces it via getMore. Asserts the legacy
// non-change-stream tailable wire path works against SecantusDB.
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

	db := cli.Database("tailable_xd")
	must(db.Drop(ctx))
	must(db.CreateCollection(ctx, "logs",
		options.CreateCollection().SetCapped(true).SetSizeInBytes(64*1024)))
	coll := db.Collection("logs")
	_, err = coll.InsertOne(ctx, bson.D{{Key: "_id", Value: int32(1)}})
	must(err)

	cursor, err := coll.Find(ctx, bson.D{},
		options.Find().SetCursorType(options.Tailable))
	must(err)
	defer cursor.Close(ctx)

	if !cursor.Next(ctx) {
		fmt.Fprintln(os.Stderr, "FAIL: expected to drain the seeded doc")
		os.Exit(1)
	}
	var d bson.M
	must(cursor.Decode(&d))
	if d["_id"] != int32(1) {
		fmt.Fprintf(os.Stderr, "FAIL: first doc _id: got %v, want 1\n", d["_id"])
		os.Exit(1)
	}

	// Insert a new doc; tailable cursor should pick it up via getMore.
	_, err = coll.InsertOne(ctx, bson.D{{Key: "_id", Value: int32(2)}})
	must(err)

	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		if cursor.TryNext(ctx) {
			must(cursor.Decode(&d))
			if d["_id"] != int32(2) {
				fmt.Fprintf(os.Stderr, "FAIL: second doc _id: got %v, want 2\n", d["_id"])
				os.Exit(1)
			}
			fmt.Println("OK")
			return
		}
		time.Sleep(100 * time.Millisecond)
	}
	fmt.Fprintln(os.Stderr, "FAIL: tailable cursor did not surface the new doc within 5s")
	os.Exit(1)
}
