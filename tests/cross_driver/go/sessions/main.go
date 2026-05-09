// Cross-driver logical-sessions smoke — Go.
//
// Drives startSession / endSessions / refreshSessions through raw
// runCommand so the wire-level lsid round-trip is what's exercised
// (not the driver's high-level Session API).
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

type startSessionReply struct {
	OK             float64 `bson:"ok"`
	TimeoutMinutes int32   `bson:"timeoutMinutes"`
	ID             struct {
		ID bson.Binary `bson:"id"`
	} `bson:"id"`
}

type okReply struct {
	OK float64 `bson:"ok"`
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

	adminDb := cli.Database("admin")

	// 1. startSession returns {id: BinData(4, uuid), timeoutMinutes}.
	var started startSessionReply
	must(adminDb.RunCommand(ctx, bson.D{{Key: "startSession", Value: 1}}).Decode(&started))
	if started.OK != 1 {
		fmt.Fprintf(os.Stderr, "startSession ok=%v\n", started.OK)
		os.Exit(1)
	}
	if started.ID.ID.Subtype != 4 {
		fmt.Fprintf(os.Stderr, "lsid subtype=%d, want 4\n", started.ID.ID.Subtype)
		os.Exit(1)
	}
	if len(started.ID.ID.Data) != 16 {
		fmt.Fprintf(os.Stderr, "lsid len=%d, want 16\n", len(started.ID.ID.Data))
		os.Exit(1)
	}
	if started.TimeoutMinutes != 30 {
		fmt.Fprintf(os.Stderr, "timeoutMinutes=%d, want 30\n", started.TimeoutMinutes)
		os.Exit(1)
	}

	// 2. endSessions on the new lsid → ok.
	var ended okReply
	must(adminDb.RunCommand(ctx, bson.D{
		{Key: "endSessions", Value: bson.A{bson.D{{Key: "id", Value: started.ID.ID}}}},
	}).Decode(&ended))
	if ended.OK != 1 {
		fmt.Fprintf(os.Stderr, "endSessions ok=%v\n", ended.OK)
		os.Exit(1)
	}

	// 3. refreshSessions implicit-creates an unknown lsid; server accepts.
	fakeLsid := bson.Binary{Subtype: 4, Data: []byte("0123456789abcdef")}
	var refreshed okReply
	must(adminDb.RunCommand(ctx, bson.D{
		{Key: "refreshSessions", Value: bson.A{bson.D{{Key: "id", Value: fakeLsid}}}},
	}).Decode(&refreshed))
	if refreshed.OK != 1 {
		fmt.Fprintf(os.Stderr, "refreshSessions ok=%v\n", refreshed.OK)
		os.Exit(1)
	}

	fmt.Println("OK")
}
