// Cross-driver batchSize:0 smoke — Node.
//
// Open a find cursor with batchSize: 0 and assert next() returns a
// doc — proves firstBatch was empty (so the doc came from a
// follow-up getMore) AND that the cursor.id was non-zero (so the
// driver knew to call getMore at all). Wraps the assertion in a
// command listener so we also verify a getMore actually flew.

const { MongoClient } = require('mongodb');

function fail(msg) {
  console.error(msg);
  process.exit(1);
}

async function main() {
  const uri = process.env.MONGODB_URI;
  if (!uri) fail('MONGODB_URI not set');

  const seenCommands = [];
  const client = new MongoClient(uri, {
    serverSelectionTimeoutMS: 5000,
    monitorCommands: true,
  });
  client.on('commandStarted', (ev) => seenCommands.push(ev.commandName));

  await client.connect();
  try {
    const coll = client.db('batch_zero_xd').collection('c');
    await coll.drop().catch(() => {});
    await coll.insertMany([0, 1, 2, 3, 4].map((i) => ({ _id: i })));

    seenCommands.length = 0;
    const cursor = coll.find({}, { batchSize: 0 });
    try {
      const first = await cursor.next();
      if (!first || first._id !== 0) {
        fail(`first doc: got ${JSON.stringify(first)}, want {_id: 0}`);
      }
    } finally {
      await cursor.close();
    }

    // The first 'find' is the open; subsequent 'getMore' is what
    // batchSize:0 forces.
    if (!seenCommands.includes('find') || !seenCommands.includes('getMore')) {
      fail(`expected find + getMore on the wire, got ${JSON.stringify(seenCommands)}`);
    }

    console.log('OK');
  } finally {
    await client.close();
  }
}

main().catch((err) => fail(`uncaught: ${err && err.stack ? err.stack : err}`));
