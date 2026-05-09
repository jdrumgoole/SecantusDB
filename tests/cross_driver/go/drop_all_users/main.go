// Cross-driver dropAllUsersFromDatabase smoke — Go.
//
// As root, provision two users on the `shop` db, run
// `dropAllUsersFromDatabase`, assert both users are gone but a
// user on a different db (`other`) is left intact, and that the
// reply carries `n: 2`.
package main

import (
	"context"
	"fmt"
	"net/url"
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
	root := authedClient(uri, "root", adminPwd)
	defer root.Disconnect(ctx)

	must(root.Database("shop").RunCommand(ctx, bson.D{
		{Key: "createUser", Value: "alice"},
		{Key: "pwd", Value: "p"},
		{Key: "roles", Value: bson.A{
			bson.D{{Key: "role", Value: "read"}, {Key: "db", Value: "shop"}},
		}},
	}).Err())
	must(root.Database("shop").RunCommand(ctx, bson.D{
		{Key: "createUser", Value: "bob"},
		{Key: "pwd", Value: "p"},
		{Key: "roles", Value: bson.A{
			bson.D{{Key: "role", Value: "readWrite"}, {Key: "db", Value: "shop"}},
		}},
	}).Err())
	must(root.Database("other").RunCommand(ctx, bson.D{
		{Key: "createUser", Value: "carol"},
		{Key: "pwd", Value: "p"},
		{Key: "roles", Value: bson.A{
			bson.D{{Key: "role", Value: "read"}, {Key: "db", Value: "other"}},
		}},
	}).Err())

	var result bson.M
	must(root.Database("shop").RunCommand(ctx,
		bson.D{{Key: "dropAllUsersFromDatabase", Value: 1}}).Decode(&result))
	if n, ok := result["n"].(int32); !ok || n != 2 {
		fmt.Fprintf(os.Stderr, "FAIL: n: got %v (%T), want int32(2)\n", result["n"], result["n"])
		os.Exit(1)
	}

	var info bson.M
	must(root.Database("shop").RunCommand(ctx,
		bson.D{{Key: "usersInfo", Value: 1}}).Decode(&info))
	if users, ok := info["users"].(bson.A); !ok || len(users) != 0 {
		fmt.Fprintf(os.Stderr, "FAIL: shop usersInfo: %v\n", info["users"])
		os.Exit(1)
	}

	must(root.Database("other").RunCommand(ctx,
		bson.D{{Key: "usersInfo", Value: 1}}).Decode(&info))
	users, _ := info["users"].(bson.A)
	if len(users) != 1 {
		fmt.Fprintf(os.Stderr, "FAIL: other db should still have 1 user, got %d\n", len(users))
		os.Exit(1)
	}

	fmt.Println("OK")
}
