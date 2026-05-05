// Cross-driver BSON type fidelity smoke — Go.
//
// Insert one document containing every BSON type whose Go-side
// representation is a distinct strict type, then find it back and
// assert each field round-trips with the same type and value:
//
//   - ObjectId       → bson.ObjectID
//   - int32          → int32  (not promoted to int64)
//   - int64          → int64  (not demoted to int32)
//   - float64        → float64
//   - Decimal128     → bson.Decimal128
//   - DateTime       → time.Time (UTC, ms precision)
//   - Binary         → bson.Binary (subtype 0)
//   - Boolean / null / nested doc / array — sanity checks
//
// SecantusDB stores BSON as opaque blobs, so any type collapse here
// would be a wire-protocol bug (the BSON encoder rejected the input
// and silently coerced) — exactly the class of bug pymongo's gauge
// can't catch because pymongo's BSON is the reference implementation.
package main

import (
	"context"
	"fmt"
	"os"
	"reflect"
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

	coll := cli.Database("types_xd").Collection("c")
	must(coll.Drop(ctx))

	// Build the input doc. Each key uses the type we want preserved.
	objID := bson.NewObjectID()
	dec, err := bson.ParseDecimal128("3.141592653589793238")
	must(err)
	when := time.Date(2026, 5, 6, 12, 34, 56, 789_000_000, time.UTC)
	bin := bson.Binary{Subtype: 0x00, Data: []byte("hello")}

	in := bson.D{
		{Key: "_id", Value: objID},
		{Key: "i32", Value: int32(2147483647)},
		{Key: "i64", Value: int64(9223372036854775807)},
		{Key: "f64", Value: float64(2.5)},
		{Key: "dec", Value: dec},
		{Key: "dt", Value: when},
		{Key: "bin", Value: bin},
		{Key: "b", Value: true},
		{Key: "n", Value: nil},
		{Key: "sub", Value: bson.D{{Key: "x", Value: int32(1)}}},
		{Key: "arr", Value: bson.A{int32(1), "two", float64(3.5)}},
	}
	_, err = coll.InsertOne(ctx, in)
	must(err)

	// Decode into a struct so each field's Go-side type is enforced
	// by the bson decoder. Mismatches surface as decode errors here,
	// not as silent type narrowing.
	type sub struct {
		X int32 `bson:"x"`
	}
	type out struct {
		ID  bson.ObjectID  `bson:"_id"`
		I32 int32          `bson:"i32"`
		I64 int64          `bson:"i64"`
		F64 float64        `bson:"f64"`
		Dec bson.Decimal128 `bson:"dec"`
		DT  time.Time      `bson:"dt"`
		Bin bson.Binary    `bson:"bin"`
		B   bool           `bson:"b"`
		N   any            `bson:"n"`
		Sub sub            `bson:"sub"`
		Arr bson.A         `bson:"arr"`
	}
	var got out
	must(coll.FindOne(ctx, bson.D{{Key: "_id", Value: objID}}).Decode(&got))

	// _id round-trip: same 12-byte value.
	if got.ID != objID {
		fmt.Fprintf(os.Stderr, "FAIL: _id: got %v, want %v\n", got.ID, objID)
		os.Exit(1)
	}
	// Numeric type fidelity — exact-equality on max int32 / int64 +
	// the chosen double would tear if the driver / server collapsed
	// to a different numeric type.
	if got.I32 != 2147483647 {
		fmt.Fprintf(os.Stderr, "FAIL: i32: got %v\n", got.I32)
		os.Exit(1)
	}
	if got.I64 != 9223372036854775807 {
		fmt.Fprintf(os.Stderr, "FAIL: i64: got %v\n", got.I64)
		os.Exit(1)
	}
	if got.F64 != 2.5 {
		fmt.Fprintf(os.Stderr, "FAIL: f64: got %v\n", got.F64)
		os.Exit(1)
	}
	if got.Dec.String() != dec.String() {
		fmt.Fprintf(os.Stderr, "FAIL: dec: got %s, want %s\n", got.Dec.String(), dec.String())
		os.Exit(1)
	}
	// time.Time round-trip — BSON DateTime is ms precision, so trim.
	wantT := when.Truncate(time.Millisecond)
	gotT := got.DT.UTC().Truncate(time.Millisecond)
	if !gotT.Equal(wantT) {
		fmt.Fprintf(os.Stderr, "FAIL: dt: got %v, want %v\n", gotT, wantT)
		os.Exit(1)
	}
	if got.Bin.Subtype != bin.Subtype || !reflect.DeepEqual(got.Bin.Data, bin.Data) {
		fmt.Fprintf(os.Stderr, "FAIL: bin: got %+v, want %+v\n", got.Bin, bin)
		os.Exit(1)
	}
	if !got.B {
		fmt.Fprintf(os.Stderr, "FAIL: b: got %v\n", got.B)
		os.Exit(1)
	}
	if got.N != nil {
		fmt.Fprintf(os.Stderr, "FAIL: n: got %v, want nil\n", got.N)
		os.Exit(1)
	}
	if got.Sub.X != 1 {
		fmt.Fprintf(os.Stderr, "FAIL: sub.x: got %v\n", got.Sub.X)
		os.Exit(1)
	}
	if len(got.Arr) != 3 {
		fmt.Fprintf(os.Stderr, "FAIL: arr len: got %d\n", len(got.Arr))
		os.Exit(1)
	}
	if v, ok := got.Arr[0].(int32); !ok || v != 1 {
		fmt.Fprintf(os.Stderr, "FAIL: arr[0]: got %T %v, want int32 1\n", got.Arr[0], got.Arr[0])
		os.Exit(1)
	}
	if v, ok := got.Arr[1].(string); !ok || v != "two" {
		fmt.Fprintf(os.Stderr, "FAIL: arr[1]: got %T %v\n", got.Arr[1], got.Arr[1])
		os.Exit(1)
	}
	if v, ok := got.Arr[2].(float64); !ok || v != 3.5 {
		fmt.Fprintf(os.Stderr, "FAIL: arr[2]: got %T %v\n", got.Arr[2], got.Arr[2])
		os.Exit(1)
	}

	fmt.Println("OK")
}
