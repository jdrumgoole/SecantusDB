// Cross-driver tailable cursor smoke — Node.

const { MongoClient } = require('mongodb');

function fail(msg) {
  console.error(msg);
  process.exit(1);
}

function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

async function main() {
  const uri = process.env.MONGODB_URI;
  if (!uri) fail('MONGODB_URI not set');

  const client = new MongoClient(uri, { serverSelectionTimeoutMS: 5000 });
  await client.connect();
  try {
    const db = client.db('tailable_xd');
    await db.dropDatabase().catch(() => {});
    await db.createCollection('logs', { capped: true, size: 64 * 1024 });
    const coll = db.collection('logs');
    await coll.insertOne({ _id: 1 });

    const cursor = coll.find({}, { tailable: true });
    try {
      // Drain the seeded doc.
      const first = await cursor.next();
      if (!first || first._id !== 1) fail(`first: ${JSON.stringify(first)}, want {_id: 1}`);

      // Insert another doc; tailable cursor should surface it.
      await coll.insertOne({ _id: 2 });

      const deadline = Date.now() + 5000;
      while (Date.now() < deadline) {
        const ev = await cursor.tryNext();
        if (ev) {
          if (ev._id !== 2) fail(`second: ${JSON.stringify(ev)}, want {_id: 2}`);
          console.log('OK');
          return;
        }
        await sleep(100);
      }
      fail('tailable cursor did not surface the new doc within 5s');
    } finally {
      await cursor.close();
    }
  } finally {
    await client.close();
  }
}

main().catch((err) => fail(`uncaught: ${err && err.stack ? err.stack : err}`));
