// Cross-driver change-stream resume smoke — Go.
//
// Workload: open a watch, drive three inserts, capture the
// resume-token after event 1, close, reopen with `resumeAfter`,
// and verify the next two events arrive in order. Then re-open
// with `startAtOperationTime` set to a timestamp captured before
// event 1 and verify all three events replay.
//
// Resume tokens are opaque to drivers but carry server-side state
// (in SecantusDB: hex-encoded BSON of `{s, t, n, k}`). If the
// driver re-presents a token verbatim, the server must accept it,
// position the stream, and stream subsequent events. Wire-shape
// divergences in token round-trip surface here as a failed resume
// or a wrong starting position.
package main

import (
	"context"
	"fmt"
	"os"
	"time"

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

// Each event's documentKey is `{_id: <value>}`. We decode the
// outer event into bson.M and the documentKey via a typed struct
// so the _id field's Go-side type is enforced regardless of
// whether the driver picks bson.M or bson.D for nested docs.
type docKey struct {
	ID int32 `bson:"_id"`
}

type changeEvent struct {
	OperationType string  `bson:"operationType"`
	DocumentKey   docKey  `bson:"documentKey"`
	ResumeToken   bson.M  `bson:"_id"`
	WallTime      bson.M  `bson:"-"`
}

func nextEvent(cs *mongo.ChangeStream, ctx context.Context, deadline time.Time) changeEvent {
	for time.Now().Before(deadline) {
		if cs.TryNext(ctx) {
			var e changeEvent
			must(cs.Decode(&e))
			return e
		}
		time.Sleep(150 * time.Millisecond)
	}
	fmt.Fprintln(os.Stderr, "FAIL: timed out waiting for next change event")
	os.Exit(1)
	return changeEvent{}
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

	coll := cli.Database("cs_resume_xd").Collection("c")
	must(coll.Drop(ctx))

	// startAtOperationTime needs an opTime <= the first oplog entry
	// we want to receive. hello.lastWrite.opTime advances on each
	// write, so capturing it BEFORE the inserts gives us a floor that
	// will replay every subsequent event.
	helloRes := cli.Database("admin").RunCommand(ctx, bson.D{{Key: "hello", Value: 1}})
	var hello bson.M
	must(helloRes.Decode(&hello))
	lastWrite, _ := hello["lastWrite"].(bson.M)
	opTime, _ := lastWrite["opTime"].(bson.M)
	startTs, _ := opTime["ts"].(bson.Timestamp)

	// 1. Open the stream and produce three events.
	cs1, err := coll.Watch(ctx, mongo.Pipeline{},
		options.ChangeStream().SetMaxAwaitTime(1*time.Second))
	must(err)
	time.Sleep(200 * time.Millisecond) // settle the cursor

	for _, id := range []int{1, 2, 3} {
		_, err = coll.InsertOne(ctx, bson.D{{Key: "_id", Value: id}})
		must(err)
	}

	deadline := time.Now().Add(8 * time.Second)
	e1 := nextEvent(cs1, ctx, deadline)
	if e1.DocumentKey.ID != 1 {
		fmt.Fprintf(os.Stderr, "FAIL: e1 _id: got %v, want 1\n", e1.DocumentKey.ID)
		os.Exit(1)
	}
	resumeAfter := e1.ResumeToken // the change event's `_id` IS the resume token
	must(cs1.Close(ctx))

	// 2. Reopen with resumeAfter from event 1; expect events 2 then 3.
	cs2, err := coll.Watch(ctx, mongo.Pipeline{},
		options.ChangeStream().
			SetResumeAfter(resumeAfter).
			SetMaxAwaitTime(1*time.Second))
	must(err)
	defer cs2.Close(ctx)

	deadline = time.Now().Add(8 * time.Second)
	e2 := nextEvent(cs2, ctx, deadline)
	e3 := nextEvent(cs2, ctx, deadline)
	if e2.DocumentKey.ID != 2 || e3.DocumentKey.ID != 3 {
		fmt.Fprintf(os.Stderr, "FAIL: resumeAfter sequence: e2=%v e3=%v, want 2,3\n",
			e2.DocumentKey.ID, e3.DocumentKey.ID)
		os.Exit(1)
	}

	// 3. Reopen with startAtOperationTime BEFORE the inserts; expect
	// all three events 1..3.
	cs3, err := coll.Watch(ctx, mongo.Pipeline{},
		options.ChangeStream().
			SetStartAtOperationTime(&startTs).
			SetMaxAwaitTime(1*time.Second))
	must(err)
	defer cs3.Close(ctx)

	deadline = time.Now().Add(8 * time.Second)
	got := []int32{}
	for len(got) < 3 && time.Now().Before(deadline) {
		if cs3.TryNext(ctx) {
			var e changeEvent
			must(cs3.Decode(&e))
			got = append(got, e.DocumentKey.ID)
		} else {
			time.Sleep(150 * time.Millisecond)
		}
	}
	if len(got) != 3 || got[0] != 1 || got[1] != 2 || got[2] != 3 {
		fmt.Fprintf(os.Stderr, "FAIL: startAtOperationTime sequence: %v, want [1 2 3]\n", got)
		os.Exit(1)
	}

	fmt.Println("OK")
}
