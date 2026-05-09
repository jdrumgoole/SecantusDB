// Cross-driver cluster-role-bundle smoke — Go.
//
// Same workload as the Node smoke: provision a clusterMonitor user
// and a backup user, verify clusterMonitor can listDatabases but
// not insert, and that backup can read every db but not insert.
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

func cmdErrCode(err error) int {
	var ce mongo.CommandError
	if err == nil {
		return 0
	}
	if e, ok := err.(mongo.CommandError); ok {
		return int(e.Code)
	}
	if _ = ce; false {
	}
	// WriteException wraps an inner error code.
	if we, ok := err.(mongo.WriteException); ok && len(we.WriteErrors) > 0 {
		return int(we.WriteErrors[0].Code)
	}
	return -1
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

	must(root.Database("admin").RunCommand(ctx, bson.D{
		{Key: "createUser", Value: "cluster_mon"},
		{Key: "pwd", Value: "p"},
		{Key: "roles", Value: bson.A{bson.D{{Key: "role", Value: "clusterMonitor"}, {Key: "db", Value: "admin"}}}},
	}).Err())
	must(root.Database("admin").RunCommand(ctx, bson.D{
		{Key: "createUser", Value: "backup_user"},
		{Key: "pwd", Value: "p"},
		{Key: "roles", Value: bson.A{bson.D{{Key: "role", Value: "backup"}, {Key: "db", Value: "admin"}}}},
	}).Err())
	_, err := root.Database("shop").Collection("items").InsertOne(ctx, bson.M{"_id": 1, "name": "thing"})
	must(err)

	// 1. clusterMonitor: listDatabases ok, insert rejected.
	cm := authedClient(uri, "cluster_mon", "p", "admin")
	defer cm.Disconnect(ctx)

	var ldb bson.M
	must(cm.Database("admin").RunCommand(ctx, bson.D{{Key: "listDatabases", Value: 1}}).Decode(&ldb))
	if dbs, ok := ldb["databases"].(bson.A); !ok || len(dbs) == 0 {
		fmt.Fprintf(os.Stderr, "clusterMonitor listDatabases: %v\n", ldb)
		os.Exit(1)
	}
	_, insErr := cm.Database("shop").Collection("items").InsertOne(ctx, bson.M{"_id": 99, "x": 1})
	if insErr == nil {
		fmt.Fprintln(os.Stderr, "clusterMonitor insert should have been rejected")
		os.Exit(1)
	}
	if c := cmdErrCode(insErr); c != 13 {
		fmt.Fprintf(os.Stderr, "clusterMonitor insert code=%d, want 13 (err=%v)\n", c, insErr)
		os.Exit(1)
	}

	// 2. backup: read every db, insert rejected.
	bk := authedClient(uri, "backup_user", "p", "admin")
	defer bk.Disconnect(ctx)

	cur, err := bk.Database("shop").Collection("items").Find(ctx, bson.D{})
	must(err)
	var docs []bson.M
	must(cur.All(ctx, &docs))
	if len(docs) != 1 {
		fmt.Fprintf(os.Stderr, "backup read: got %d docs, want 1\n", len(docs))
		os.Exit(1)
	}
	_, insErr2 := bk.Database("shop").Collection("items").InsertOne(ctx, bson.M{"_id": 99, "x": 1})
	if insErr2 == nil {
		fmt.Fprintln(os.Stderr, "backup insert should have been rejected")
		os.Exit(1)
	}
	if c := cmdErrCode(insErr2); c != 13 {
		fmt.Fprintf(os.Stderr, "backup insert code=%d, want 13 (err=%v)\n", c, insErr2)
		os.Exit(1)
	}

	fmt.Println("OK")
}
