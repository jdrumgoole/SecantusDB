// Cross-driver listDatabases filter smoke — Go.
//
// Insert one doc into three different databases, then call
// `listDatabases` with a `{name: "alpha"}` filter. Real mongod
// (and SecantusDB) returns only the matching db descriptor;
// without server-side filter support the response would carry
// every db.
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

	for _, db := range []string{"alpha", "beta", "gamma"} {
		_, err := cli.Database(db).Collection("c").InsertOne(ctx, bson.D{{Key: "_id", Value: 1}})
		must(err)
	}
	defer func() {
		for _, db := range []string{"alpha", "beta", "gamma"} {
			_ = cli.Database(db).Drop(ctx)
		}
	}()

	res, err := cli.ListDatabases(ctx, bson.D{{Key: "name", Value: "alpha"}})
	must(err)
	gotNames := []string{}
	for _, d := range res.Databases {
		gotNames = append(gotNames, d.Name)
	}
	if len(gotNames) != 1 || gotNames[0] != "alpha" {
		fmt.Fprintf(os.Stderr, "FAIL: filter {name: alpha}: got %v, want [alpha]\n", gotNames)
		os.Exit(1)
	}

	// nameOnly: true should strip sizeOnDisk/empty.
	res, err = cli.ListDatabases(ctx, bson.D{}, options.ListDatabases().SetNameOnly(true))
	must(err)
	if len(res.Databases) < 3 {
		fmt.Fprintf(os.Stderr, "FAIL: nameOnly: got %d dbs, want >= 3\n", len(res.Databases))
		os.Exit(1)
	}

	fmt.Println("OK")
}
