// Cross-driver bulk-write smoke — Node (mongo-node-driver).
//
// Same workload as tests/cross_driver/go/bulk/main.go: one mixed
// bulkWrite that insert/update/upsert/replace/delete-walks across
// six docs, then assert the result counts and final collection
// state. node-driver's bulk command builder is a separate
// implementation from pymongo's, so wire-shape divergences trip
// here.

const { MongoClient } = require('mongodb');

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
    const coll = client.db('bulk_xd').collection('c');
    await coll.drop().catch(() => {});

    await coll.insertMany([
      { _id: 1, kind: 'old' },
      { _id: 2, kind: 'old' },
    ]);

    const res = await coll.bulkWrite([
      { insertOne: { document: { _id: 3, kind: 'fresh' } } },
      { updateOne: { filter: { _id: 1 }, update: { $set: { kind: 'new' } } } },
      { updateMany: { filter: { kind: 'old' }, update: { $set: { kind: 'new' } } } },
      { replaceOne: { filter: { _id: 3 }, replacement: { _id: 3, kind: 'replaced' } } },
      {
        updateOne: {
          filter: { _id: 99 },
          update: { $set: { kind: 'upserted' } },
          upsert: true,
        },
      },
      { deleteOne: { filter: { _id: 2 } } },
    ]);

    if (res.insertedCount !== 1) fail(`insertedCount: got ${res.insertedCount}, want 1`);
    if (res.matchedCount !== 3) fail(`matchedCount: got ${res.matchedCount}, want 3`);
    if (res.modifiedCount !== 3) fail(`modifiedCount: got ${res.modifiedCount}, want 3`);
    if (res.upsertedCount !== 1) fail(`upsertedCount: got ${res.upsertedCount}, want 1`);
    if (res.deletedCount !== 1) fail(`deletedCount: got ${res.deletedCount}, want 1`);

    const got = await coll.find({}).sort({ _id: 1 }).toArray();
    if (got.length !== 3) fail(`final docs: got ${got.length}, want 3`);
    if (got[0]._id !== 1 || got[0].kind !== 'new')
      fail(`doc[0]: got ${JSON.stringify(got[0])}`);
    if (got[1]._id !== 3 || got[1].kind !== 'replaced')
      fail(`doc[1]: got ${JSON.stringify(got[1])}`);
    if (got[2]._id !== 99 || got[2].kind !== 'upserted')
      fail(`doc[2]: got ${JSON.stringify(got[2])}`);

    console.log('OK');
  } finally {
    await client.close();
  }
}

main().catch((err) => fail(`uncaught: ${err && err.stack ? err.stack : err}`));
