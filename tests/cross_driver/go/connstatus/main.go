// Cross-driver connectionStatus smoke — Go.
//
// Authenticates as a user with a known role binding and verifies
// `connectionStatus.authInfo.authenticatedUserRoles` surfaces it.
// Admin tooling reads this; a wire shape divergence here would silently
// hide effective privileges from operators.
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

type userRef struct {
	User string `bson:"user"`
	DB   string `bson:"db"`
}

type roleRef struct {
	Role string `bson:"role"`
	DB   string `bson:"db"`
}

type authInfoT struct {
	AuthenticatedUsers     []userRef `bson:"authenticatedUsers"`
	AuthenticatedUserRoles []roleRef `bson:"authenticatedUserRoles"`
}

type connStatusT struct {
	AuthInfo authInfoT `bson:"authInfo"`
	Ok       float64   `bson:"ok"`
}

func main() {
	uri := os.Getenv("MONGODB_URI")
	adminPwd := os.Getenv("ADMIN_PASSWORD")
	if uri == "" || adminPwd == "" {
		fmt.Fprintln(os.Stderr, "MONGODB_URI and ADMIN_PASSWORD required")
		os.Exit(2)
	}
	ctx := context.Background()
	parsed, err := url.Parse(uri)
	must(err)
	parsed.User = url.UserPassword("root", adminPwd)
	q := parsed.Query()
	q.Set("authSource", "admin")
	q.Set("authMechanism", "SCRAM-SHA-256")
	parsed.RawQuery = q.Encode()
	cli, err := mongo.Connect(options.Client().ApplyURI(parsed.String()))
	must(err)
	defer cli.Disconnect(ctx)

	var status connStatusT
	must(cli.Database("admin").RunCommand(ctx,
		bson.D{{Key: "connectionStatus", Value: 1}}).Decode(&status))

	if len(status.AuthInfo.AuthenticatedUsers) == 0 {
		fmt.Fprintf(os.Stderr, "FAIL: authenticatedUsers empty: %+v\n", status)
		os.Exit(1)
	}
	if status.AuthInfo.AuthenticatedUsers[0].User != "root" {
		fmt.Fprintf(os.Stderr, "FAIL: expected user=root, got %+v\n", status.AuthInfo.AuthenticatedUsers[0])
		os.Exit(1)
	}
	if len(status.AuthInfo.AuthenticatedUserRoles) == 0 {
		fmt.Fprintf(os.Stderr, "FAIL: authenticatedUserRoles empty: %+v\n", status)
		os.Exit(1)
	}
	if status.AuthInfo.AuthenticatedUserRoles[0].Role != "root" {
		fmt.Fprintf(os.Stderr, "FAIL: expected role=root, got %+v\n", status.AuthInfo.AuthenticatedUserRoles[0])
		os.Exit(1)
	}
	fmt.Println("OK")
}
