// Cross-driver updateUser smoke — Go.
//
// As root, provision a user with one password, rotate it via
// updateUser, and verify:
//   - new auth attempt with the OLD password is rejected
//   - new auth attempt with the NEW password succeeds
//
// Verifies the wire flow: derive_credentials → SCRAM-SHA-256
// stored-key replacement → next saslStart sees the new salt.
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

func authed(uri, user, pwd string) *mongo.Client {
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

	root := authed(uri, "root", adminPwd)
	defer root.Disconnect(ctx)

	// Provision alice with `orig` password.
	must(root.Database("admin").RunCommand(ctx, bson.D{
		{Key: "createUser", Value: "alice_xd"},
		{Key: "pwd", Value: "orig"},
		{Key: "roles", Value: bson.A{
			bson.D{{Key: "role", Value: "read"}, {Key: "db", Value: "admin"}},
		}},
	}).Err())

	// Rotate via updateUser.
	must(root.Database("admin").RunCommand(ctx, bson.D{
		{Key: "updateUser", Value: "alice_xd"},
		{Key: "pwd", Value: "rotated"},
	}).Err())

	// Old password — should fail.
	cliOld := authed(uri, "alice_xd", "orig")
	err := cliOld.Database("admin").RunCommand(ctx, bson.D{{Key: "ping", Value: 1}}).Err()
	cliOld.Disconnect(ctx)
	if err == nil {
		fmt.Fprintln(os.Stderr, "FAIL: old password still works")
		os.Exit(1)
	}

	// New password — should succeed.
	cliNew := authed(uri, "alice_xd", "rotated")
	defer cliNew.Disconnect(ctx)
	must(cliNew.Database("admin").RunCommand(ctx, bson.D{{Key: "ping", Value: 1}}).Err())

	fmt.Println("OK")
}
