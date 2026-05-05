// Cross-driver RBAC smoke — Go.
//
// Provisions a `read`-bound user, authenticates as that user, and
// asserts insert is rejected with code 13 / Unauthorized while find
// succeeds. Verifies the wire-level RBAC enforcement looks right
// from go-driver's perspective: the error code mapping in
// command.go's `extractError` has to recognise `Unauthorized` for the
// caller-side `MongoCommandException` shape to surface correctly.
//
// Reads the SecantusDB URI from $MONGODB_URI (no auth — we do
// the SCRAM round-trip ourselves via the driver's URI options).
// Reads the bootstrap admin password from $ADMIN_PASSWORD (the
// orchestrator fixture seeds a `root` user under that password).
package main

import (
	"context"
	"fmt"
	"net/url"
	"os"
	"strings"

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

func authedClient(uri, user, pwd string) *mongo.Client {
	parsed, err := url.Parse(uri)
	must(err)
	parsed.User = url.UserPassword(user, pwd)
	q := parsed.Query()
	q.Set("authSource", "admin")
	q.Set("authMechanism", "SCRAM-SHA-256")
	parsed.RawQuery = q.Encode()
	cli, err := mongo.Connect(options.Client().ApplyURI(parsed.String()))
	must(err)
	return cli
}

func main() {
	uri := os.Getenv("MONGODB_URI")
	adminPwd := os.Getenv("ADMIN_PASSWORD")
	if uri == "" || adminPwd == "" {
		fmt.Fprintln(os.Stderr, "MONGODB_URI and ADMIN_PASSWORD required")
		os.Exit(2)
	}
	ctx := context.Background()

	// 1. As root, provision a read-only user on the `shop` db.
	root := authedClient(uri, "root", adminPwd)
	defer root.Disconnect(ctx)
	must(root.Database("shop").RunCommand(ctx, bson.D{
		{Key: "createUser", Value: "viewer"},
		{Key: "pwd", Value: "vp"},
		{Key: "roles", Value: bson.A{
			bson.D{{Key: "role", Value: "read"}, {Key: "db", Value: "shop"}},
		}},
	}).Err())

	// 2. As viewer, find succeeds.
	viewer := authedClientFor(uri, "viewer", "vp", "shop")
	defer viewer.Disconnect(ctx)
	cur, err := viewer.Database("shop").Collection("items").Find(ctx, bson.D{})
	must(err)
	cur.Close(ctx)

	// 3. As viewer, insert is rejected with code 13.
	_, err = viewer.Database("shop").Collection("items").InsertOne(ctx, bson.D{{Key: "x", Value: 1}})
	if err == nil {
		fmt.Fprintln(os.Stderr, "FAIL: insert should have been rejected")
		os.Exit(1)
	}
	// Expect MongoCommandException with code 13 (Unauthorized).
	if !strings.Contains(err.Error(), "Unauthorized") && !strings.Contains(err.Error(), "(13)") {
		fmt.Fprintf(os.Stderr, "FAIL: unexpected error: %v\n", err)
		os.Exit(1)
	}
	fmt.Println("OK")
}

func authedClientFor(uri, user, pwd, authSource string) *mongo.Client {
	parsed, err := url.Parse(uri)
	must(err)
	parsed.User = url.UserPassword(user, pwd)
	q := parsed.Query()
	q.Set("authSource", authSource)
	q.Set("authMechanism", "SCRAM-SHA-256")
	parsed.RawQuery = q.Encode()
	cli, err := mongo.Connect(options.Client().ApplyURI(parsed.String()))
	must(err)
	return cli
}
