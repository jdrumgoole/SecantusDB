// Cross-driver custom roles smoke — Go.
//
// As root: createRole "shopAuditor" with `find` on shop, create a
// user bound to that role, then authenticate as the user and verify
// `find` succeeds while `insert` is rejected (action not in the
// role's privileges). Then grantPrivilegesToRole adds insert on top
// of find — a fresh connection sees the new privilege fire.
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

func authedClient(uri, user, pwd, authSource string) *mongo.Client {
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

func main() {
	uri := os.Getenv("MONGODB_URI")
	adminPwd := os.Getenv("ADMIN_PASSWORD")
	if uri == "" || adminPwd == "" {
		fmt.Fprintln(os.Stderr, "MONGODB_URI and ADMIN_PASSWORD required")
		os.Exit(2)
	}
	ctx := context.Background()

	root := authedClient(uri, "root", adminPwd, "admin")
	defer root.Disconnect(ctx)
	must(root.Database("shop").RunCommand(ctx, bson.D{
		{Key: "createRole", Value: "shopAuditor"},
		{Key: "privileges", Value: bson.A{
			bson.D{
				{Key: "resource", Value: bson.D{
					{Key: "db", Value: "shop"}, {Key: "collection", Value: ""},
				}},
				{Key: "actions", Value: bson.A{"find"}},
			},
		}},
		{Key: "roles", Value: bson.A{}},
	}).Err())
	must(root.Database("shop").RunCommand(ctx, bson.D{
		{Key: "createUser", Value: "auditor_go"},
		{Key: "pwd", Value: "p"},
		{Key: "roles", Value: bson.A{
			bson.D{{Key: "role", Value: "shopAuditor"}, {Key: "db", Value: "shop"}},
		}},
	}).Err())

	auditor := authedClient(uri, "auditor_go", "p", "shop")
	defer auditor.Disconnect(ctx)
	cur, err := auditor.Database("shop").Collection("items").Find(ctx, bson.D{})
	must(err)
	cur.Close(ctx)

	_, err = auditor.Database("shop").Collection("items").InsertOne(ctx, bson.D{{Key: "x", Value: 1}})
	if err == nil {
		fmt.Fprintln(os.Stderr, "FAIL: insert should have been rejected for find-only role")
		os.Exit(1)
	}
	if !strings.Contains(err.Error(), "Unauthorized") && !strings.Contains(err.Error(), "(13)") {
		fmt.Fprintf(os.Stderr, "FAIL: unexpected error: %v\n", err)
		os.Exit(1)
	}

	// grantPrivilegesToRole adds insert; reconnect to pick up.
	must(root.Database("shop").RunCommand(ctx, bson.D{
		{Key: "grantPrivilegesToRole", Value: "shopAuditor"},
		{Key: "privileges", Value: bson.A{
			bson.D{
				{Key: "resource", Value: bson.D{
					{Key: "db", Value: "shop"}, {Key: "collection", Value: ""},
				}},
				{Key: "actions", Value: bson.A{"insert"}},
			},
		}},
	}).Err())
	auditor2 := authedClient(uri, "auditor_go", "p", "shop")
	defer auditor2.Disconnect(ctx)
	_, err = auditor2.Database("shop").Collection("items").InsertOne(ctx, bson.D{{Key: "x", Value: 2}})
	must(err)

	fmt.Println("OK")
}
