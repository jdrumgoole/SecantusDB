// Cross-driver DDL change-stream smoke — Node (mongo-node-driver).
//
// Mirrors tests/cross_driver/go/ddl/main.go. Opens a watch on a
// collection, performs createIndex + dropIndex, and asserts the
// resulting events come back as `createIndexes` / `dropIndexes`
// operationType strings.

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
    const coll = client.db('ddl_xd').collection('c');
    await coll.drop().catch(() => {});
    await coll.insertOne({ _id: 1 });

    const stream = coll.watch([], { maxAwaitTimeMS: 2000, showExpandedEvents: true });
    const events = [];
    stream.on('change', (e) => {
      events.push(e.operationType);
    });

    // Settle so the change-stream cursor is registered before the writes.
    await new Promise((r) => setTimeout(r, 300));

    await coll.createIndex({ x: 1 });
    await coll.dropIndex('x_1');

    const deadline = Date.now() + 8000;
    while (Date.now() < deadline && events.length < 2) {
      await new Promise((r) => setTimeout(r, 200));
    }
    await stream.close();

    if (
      events.length !== 2 ||
      events[0] !== 'createIndexes' ||
      events[1] !== 'dropIndexes'
    ) {
      fail(`got ${JSON.stringify(events)}, want ["createIndexes","dropIndexes"]`);
    }
    console.log('OK');
  } finally {
    await client.close();
  }
}

main().catch((err) => fail(`uncaught: ${err && err.stack ? err.stack : err}`));
