// Cross-driver bulk-write smoke — Go.
//
// mongo-go-driver's BulkWrite folds a heterogeneous slice of write
// models (Insert / Update / Delete) into a single OP_MSG with a
// kind-1 documentSequence. SecantusDB's command dispatcher
// reconstructs that shape from the wire and routes to per-op
// handlers. The smoke exercises the fold/unfold path with one
// document per op type so any wire-shape divergence trips here.
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
		fmt.Fprintln(os.Stderr, "MONGODB_URI not set")
		os.Exit(2)
	}
	ctx := context.Background()
	cli, err := mongo.Connect(options.Client().ApplyURI(uri))
	must(err)
	defer cli.Disconnect(ctx)

	coll := cli.Database("bulk_xd").Collection("c")
	must(coll.Drop(ctx))

	// Seed two docs we'll target with update + delete in the bulk.
	_, err = coll.InsertMany(ctx, []interface{}{
		bson.D{{Key: "_id", Value: 1}, {Key: "kind", Value: "old"}},
		bson.D{{Key: "_id", Value: 2}, {Key: "kind", Value: "old"}},
	})
	must(err)

	// One mixed bulk: insert + update one + update many + delete one
	// + replace one + upsert. The driver folds the lot into a single
	// OP_MSG; the server fans them out to insert/update/delete handlers.
	models := []mongo.WriteModel{
		mongo.NewInsertOneModel().SetDocument(bson.D{
			{Key: "_id", Value: 3}, {Key: "kind", Value: "fresh"},
		}),
		mongo.NewUpdateOneModel().
			SetFilter(bson.D{{Key: "_id", Value: 1}}).
			SetUpdate(bson.D{{Key: "$set", Value: bson.D{{Key: "kind", Value: "new"}}}}),
		mongo.NewUpdateManyModel().
			SetFilter(bson.D{{Key: "kind", Value: "old"}}).
			SetUpdate(bson.D{{Key: "$set", Value: bson.D{{Key: "kind", Value: "new"}}}}),
		mongo.NewReplaceOneModel().
			SetFilter(bson.D{{Key: "_id", Value: 3}}).
			SetReplacement(bson.D{{Key: "_id", Value: 3}, {Key: "kind", Value: "replaced"}}),
		mongo.NewUpdateOneModel().
			SetFilter(bson.D{{Key: "_id", Value: 99}}).
			SetUpdate(bson.D{{Key: "$set", Value: bson.D{{Key: "kind", Value: "upserted"}}}}).
			SetUpsert(true),
		mongo.NewDeleteOneModel().SetFilter(bson.D{{Key: "_id", Value: 2}}),
	}
	res, err := coll.BulkWrite(ctx, models)
	must(err)

	if res.InsertedCount != 1 {
		fmt.Fprintf(os.Stderr, "FAIL: insertedCount: got %d, want 1\n", res.InsertedCount)
		os.Exit(1)
	}
	// UpdateOne hits doc 1 (already 'old' → 'new'), UpdateMany hits
	// doc 2 (still 'old' → 'new'). ReplaceOne always counts modified.
	if res.MatchedCount != 3 || res.ModifiedCount != 3 {
		fmt.Fprintf(os.Stderr, "FAIL: matched=%d modified=%d, want 3/3\n",
			res.MatchedCount, res.ModifiedCount)
		os.Exit(1)
	}
	if res.UpsertedCount != 1 {
		fmt.Fprintf(os.Stderr, "FAIL: upsertedCount: got %d, want 1\n", res.UpsertedCount)
		os.Exit(1)
	}
	if res.DeletedCount != 1 {
		fmt.Fprintf(os.Stderr, "FAIL: deletedCount: got %d, want 1\n", res.DeletedCount)
		os.Exit(1)
	}

	// Verify final state: docs 1, 3 (replaced), 99 (upserted) — 2 was deleted.
	cur, err := coll.Find(ctx, bson.D{}, options.Find().SetSort(bson.D{{Key: "_id", Value: 1}}))
	must(err)
	var got []bson.M
	must(cur.All(ctx, &got))
	if len(got) != 3 {
		fmt.Fprintf(os.Stderr, "FAIL: final docs: got %d, want 3\n", len(got))
		os.Exit(1)
	}
	if got[0]["_id"] != int32(1) || got[0]["kind"] != "new" {
		fmt.Fprintf(os.Stderr, "FAIL: doc[0]: got %+v\n", got[0])
		os.Exit(1)
	}
	if got[1]["_id"] != int32(3) || got[1]["kind"] != "replaced" {
		fmt.Fprintf(os.Stderr, "FAIL: doc[1]: got %+v\n", got[1])
		os.Exit(1)
	}
	if got[2]["_id"] != int32(99) || got[2]["kind"] != "upserted" {
		fmt.Fprintf(os.Stderr, "FAIL: doc[2]: got %+v\n", got[2])
		os.Exit(1)
	}

	fmt.Println("OK")
}
