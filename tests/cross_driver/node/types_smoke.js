// Cross-driver BSON type fidelity smoke — Node (mongo-node-driver).
//
// Same workload as tests/cross_driver/go/types/main.go: insert one
// document containing every BSON type whose JS-side representation is
// a distinct strict class, then find it back and assert each field
// preserved its class and value. node-driver's BSON layer
// (`bson` npm package) is a separate implementation from pymongo's,
// so any wire-shape divergence between SecantusDB and a real mongod
// trips here with a class-mismatch.

const { MongoClient, ObjectId, Decimal128, Long, Binary, Int32, Double } = require('mongodb');

function fail(msg) {
  console.error(msg);
  process.exit(1);
}

async function main() {
  const uri = process.env.MONGODB_URI;
  if (!uri) fail('MONGODB_URI not set');

  const client = new MongoClient(uri, { serverSelectionTimeoutMS: 5000 });
  await client.connect();
  try {
    const coll = client.db('types_xd').collection('c');
    await coll.drop().catch(() => {});

    const objID = new ObjectId();
    const dec = Decimal128.fromString('3.141592653589793238');
    const when = new Date('2026-05-06T12:34:56.789Z');
    const bin = new Binary(Buffer.from('hello'), 0);

    // Force int32 vs int64 explicitly — JS's Number is 64-bit float, so
    // SecantusDB must respect the BSON-level int32 / int64 tag we send,
    // not silently re-tag it from value alone.
    const doc = {
      _id: objID,
      i32: new Int32(2147483647),
      i64: Long.fromString('9223372036854775807'),
      f64: new Double(2.5),
      dec,
      dt: when,
      bin,
      b: true,
      n: null,
      sub: { x: new Int32(1) },
      arr: [new Int32(1), 'two', new Double(3.5)],
    };

    await coll.insertOne(doc);

    // Use { promoteValues: false } at the collection level so int32 /
    // int64 / Double come back as their wrapper classes rather than
    // collapsed to JS Number — the whole point of the test.
    const collTyped = client
      .db('types_xd')
      .collection('c', { promoteValues: false, promoteLongs: false });
    const got = await collTyped.findOne({ _id: objID });
    if (!got) fail('findOne returned null');

    if (!(got._id instanceof ObjectId) || !got._id.equals(objID)) {
      fail(`_id: got ${got._id} (${got._id?.constructor?.name})`);
    }
    if (!(got.i32 instanceof Int32) || got.i32.valueOf() !== 2147483647) {
      fail(`i32: got ${got.i32} (${got.i32?.constructor?.name})`);
    }
    if (!(got.i64 instanceof Long) || got.i64.toString() !== '9223372036854775807') {
      fail(`i64: got ${got.i64} (${got.i64?.constructor?.name})`);
    }
    if (!(got.f64 instanceof Double) || got.f64.valueOf() !== 2.5) {
      fail(`f64: got ${got.f64} (${got.f64?.constructor?.name})`);
    }
    if (!(got.dec instanceof Decimal128) || got.dec.toString() !== dec.toString()) {
      fail(`dec: got ${got.dec} (${got.dec?.constructor?.name})`);
    }
    if (!(got.dt instanceof Date) || got.dt.getTime() !== when.getTime()) {
      fail(`dt: got ${got.dt}`);
    }
    if (
      !(got.bin instanceof Binary) ||
      got.bin.sub_type !== 0 ||
      Buffer.from(got.bin.value()).toString('utf8') !== 'hello'
    ) {
      fail(`bin: got ${JSON.stringify(got.bin)}`);
    }
    if (got.b !== true) fail(`b: got ${got.b}`);
    if (got.n !== null) fail(`n: got ${got.n}`);
    if (!(got.sub.x instanceof Int32) || got.sub.x.valueOf() !== 1) {
      fail(`sub.x: got ${got.sub.x} (${got.sub.x?.constructor?.name})`);
    }
    if (!Array.isArray(got.arr) || got.arr.length !== 3) {
      fail(`arr: got ${JSON.stringify(got.arr)}`);
    }
    if (!(got.arr[0] instanceof Int32) || got.arr[0].valueOf() !== 1) {
      fail(`arr[0]: got ${got.arr[0]} (${got.arr[0]?.constructor?.name})`);
    }
    if (got.arr[1] !== 'two') fail(`arr[1]: got ${got.arr[1]}`);
    if (!(got.arr[2] instanceof Double) || got.arr[2].valueOf() !== 3.5) {
      fail(`arr[2]: got ${got.arr[2]} (${got.arr[2]?.constructor?.name})`);
    }

    console.log('OK');
  } finally {
    await client.close();
  }
}

main().catch((err) => fail(`uncaught: ${err && err.stack ? err.stack : err}`));
