// Cross-driver listDatabases filter smoke — Node (mongo-node-driver).
//
// Insert one doc into three databases, then list with
// `{name: "alpha"}` filter — should return only that one.

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
    for (const dbName of ['alpha', 'beta', 'gamma']) {
      await client.db(dbName).collection('c').insertOne({ _id: 1 });
    }
    try {
      const filtered = await client.db().admin().listDatabases({ filter: { name: 'alpha' } });
      const names = filtered.databases.map((d) => d.name);
      if (names.length !== 1 || names[0] !== 'alpha') {
        fail(`filter: got ${JSON.stringify(names)}, want ['alpha']`);
      }

      const namesOnly = await client.db().admin().listDatabases({ nameOnly: true });
      if (namesOnly.databases.length < 3) {
        fail(`nameOnly: got ${namesOnly.databases.length} dbs, want >= 3`);
      }

      console.log('OK');
    } finally {
      for (const dbName of ['alpha', 'beta', 'gamma']) {
        await client.db(dbName).dropDatabase().catch(() => {});
      }
    }
  } finally {
    await client.close();
  }
}

main().catch((err) => fail(`uncaught: ${err && err.stack ? err.stack : err}`));
