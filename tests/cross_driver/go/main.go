// Cross-driver geo smoke test — Go.
//
// Runs the same canonical geo workload that tests/test_geo_query.py
// drives via pymongo, but through the official mongo-go-driver. Goal is
// catching wire-protocol bugs that surface only with go-driver's BSON
// serialization — distance-field type mismatches, GeoJSON envelope
// quirks, etc.
//
// Reads the SecantusDB URI from $MONGODB_URI. Exits 0 on success;
// prints the failure and exits non-zero on any assertion miss.
package main

import (
	"context"
	"fmt"
	"os"
	"reflect"

	"go.mongodb.org/mongo-driver/v2/bson"
	"go.mongodb.org/mongo-driver/v2/mongo"
	"go.mongodb.org/mongo-driver/v2/mongo/options"
)

func main() {
	uri := os.Getenv("MONGODB_URI")
	if uri == "" {
		fmt.Fprintln(os.Stderr, "MONGODB_URI not set")
		os.Exit(2)
	}
	ctx := context.Background()
	client, err := mongo.Connect(options.Client().ApplyURI(uri))
	must(err)
	defer client.Disconnect(ctx)

	coll := client.Database("geo_xdriver").Collection("places")
	must(coll.Drop(ctx))

	// Three GeoJSON Points: one at the origin, one ~111 m east, one far.
	docs := []interface{}{
		bson.D{
			{Key: "_id", Value: int32(1)},
			{Key: "loc", Value: bson.D{
				{Key: "type", Value: "Point"},
				{Key: "coordinates", Value: bson.A{0.0, 0.0}},
			}},
		},
		bson.D{
			{Key: "_id", Value: int32(2)},
			{Key: "loc", Value: bson.D{
				{Key: "type", Value: "Point"},
				{Key: "coordinates", Value: bson.A{0.001, 0.0}},
			}},
		},
		bson.D{
			{Key: "_id", Value: int32(3)},
			{Key: "loc", Value: bson.D{
				{Key: "type", Value: "Point"},
				{Key: "coordinates", Value: bson.A{50.0, 50.0}},
			}},
		},
	}
	_, err = coll.InsertMany(ctx, docs)
	must(err)

	// 2dsphere index.
	_, err = coll.Indexes().CreateOne(
		ctx,
		mongo.IndexModel{Keys: bson.D{{Key: "loc", Value: "2dsphere"}}},
	)
	must(err)

	// $geoWithin via $centerSphere — same query the pymongo tests use.
	// 0.001 rad ≈ 6.4 km — should match docs 1 and 2.
	filter := bson.D{
		{Key: "loc", Value: bson.D{
			{Key: "$geoWithin", Value: bson.D{
				{Key: "$centerSphere", Value: bson.A{
					bson.A{0.0, 0.0},
					0.001,
				}},
			}},
		}},
	}
	cursor, err := coll.Find(ctx, filter)
	must(err)
	var results []bson.M
	must(cursor.All(ctx, &results))

	got := []int32{}
	for _, doc := range results {
		got = append(got, doc["_id"].(int32))
	}
	want := []int32{1, 2}
	// $geoWithin doesn't guarantee order; compare as sets.
	if !setEqual(got, want) {
		fmt.Fprintf(os.Stderr, "$geoWithin: got %v, want %v\n", got, want)
		os.Exit(1)
	}

	// $geoNear — both ranks and attaches distance.
	pipeline := mongo.Pipeline{
		bson.D{{Key: "$geoNear", Value: bson.D{
			{Key: "near", Value: bson.D{
				{Key: "type", Value: "Point"},
				{Key: "coordinates", Value: bson.A{0.0, 0.0}},
			}},
			{Key: "distanceField", Value: "d"},
			{Key: "key", Value: "loc"},
			{Key: "maxDistance", Value: 200.0},
		}}},
	}
	aggCursor, err := coll.Aggregate(ctx, pipeline)
	must(err)
	var aggResults []bson.M
	must(aggCursor.All(ctx, &aggResults))

	gotIds := []int32{}
	for _, doc := range aggResults {
		gotIds = append(gotIds, doc["_id"].(int32))
	}
	if !reflect.DeepEqual(gotIds, []int32{1, 2}) {
		fmt.Fprintf(os.Stderr, "$geoNear order: got %v, want [1 2]\n", gotIds)
		os.Exit(1)
	}
	// First doc at distance 0; second ≈ 111 m.
	d0, _ := aggResults[0]["d"].(float64)
	d1, _ := aggResults[1]["d"].(float64)
	if d0 > 0.001 {
		fmt.Fprintf(os.Stderr, "$geoNear d[0]: got %v, want ~0\n", d0)
		os.Exit(1)
	}
	if d1 < 100 || d1 > 130 {
		fmt.Fprintf(os.Stderr, "$geoNear d[1]: got %v, want ~111\n", d1)
		os.Exit(1)
	}

	fmt.Println("OK")
}

func must(err error) {
	if err != nil {
		fmt.Fprintf(os.Stderr, "fatal: %v\n", err)
		os.Exit(1)
	}
}

func setEqual(a, b []int32) bool {
	if len(a) != len(b) {
		return false
	}
	m := map[int32]bool{}
	for _, v := range a {
		m[v] = true
	}
	for _, v := range b {
		if !m[v] {
			return false
		}
	}
	return true
}
