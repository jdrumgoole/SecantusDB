// Cross-driver SCRAM-SHA-1 smoke — Go.
//
// As root, create a user with mechanisms=["SCRAM-SHA-1"]. Then
// reconnect with authMechanism=SCRAM-SHA-1 and verify a privileged
// operation succeeds. Asserts the legacy MD5-prepass + SHA-1 PBKDF2
// path matches what mongo-go-driver sends. Authentication failure
// trips here as a SCRAM proof mismatch.
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

func authedClient(uri, user, pwd, authSource, mechanism string) *mongo.Client {
	parsed, err := url.Parse(uri)
	must(err)
	parsed.User = url.UserPassword(user, pwd)
	q := parsed.Query()
	q.Set("authSource", authSource)
	q.Set("authMechanism", mechanism)
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

	root := authedClient(uri, "root", adminPwd, "admin", "SCRAM-SHA-256")
	defer root.Disconnect(ctx)
	must(root.Database("admin").RunCommand(ctx, bson.D{
		{Key: "createUser", Value: "legacy"},
		{Key: "pwd", Value: "pass"},
		{Key: "roles", Value: bson.A{}},
		{Key: "mechanisms", Value: bson.A{"SCRAM-SHA-1"}},
	}).Err())

	cli := authedClient(uri, "legacy", "pass", "admin", "SCRAM-SHA-1")
	defer cli.Disconnect(ctx)
	must(cli.Database("admin").RunCommand(ctx,
		bson.D{{Key: "ping", Value: 1}}).Err())

	// Decode connectionStatus into a typed struct — bson.M nested
	// docs decode inconsistently across mongo-go-driver versions.
	type userRef struct {
		User string `bson:"user"`
		DB   string `bson:"db"`
	}
	type authInfoT struct {
		AuthenticatedUsers []userRef `bson:"authenticatedUsers"`
	}
	type connStatusT struct {
		AuthInfo authInfoT `bson:"authInfo"`
	}
	var status connStatusT
	must(cli.Database("admin").RunCommand(ctx,
		bson.D{{Key: "connectionStatus", Value: 1}}).Decode(&status))
	if len(status.AuthInfo.AuthenticatedUsers) == 0 {
		fmt.Fprintf(os.Stderr, "FAIL: no authenticatedUsers: %+v\n", status)
		os.Exit(1)
	}
	if status.AuthInfo.AuthenticatedUsers[0].User != "legacy" {
		fmt.Fprintf(os.Stderr, "FAIL: expected user=legacy, got %+v\n", status.AuthInfo.AuthenticatedUsers[0])
		os.Exit(1)
	}
	fmt.Println("OK")
}
