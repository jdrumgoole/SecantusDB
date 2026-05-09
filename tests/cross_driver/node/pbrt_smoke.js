// Cross-driver postBatchResumeToken smoke — Node.

const { MongoClient } = require('mongodb');

function fail(msg) {
  console.error(msg);
  process.exit(1);
}

function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

function tokenSig(token) {
  if (!token) return '';
  // Resume tokens are `{_data: <hex>}` — compare on _data.
  return typeof token._data === 'string' ? token._data : JSON.stringify(token);
}

async function main() {
  const uri = process.env.MONGODB_URI;
  if (!uri) fail('MONGODB_URI not set');

  const client = new MongoClient(uri, { serverSelectionTimeoutMS: 5000 });
  await client.connect();
  try {
    const coll = client.db('pbrt_xd').collection('c');
    await coll.drop().catch(() => {});

    const cs = coll.watch([], { maxAwaitTimeMS: 500 });
    try {
      // Pin the cursor before any inserts; subsequent tryNext() pulls
      // empty batches, but the resume token should still advance.
      const initial = tokenSig(cs.resumeToken);
      await cs.tryNext();
      await sleep(200);
      await cs.tryNext();
      await sleep(200);
      await cs.tryNext();
      const after = tokenSig(cs.resumeToken);

      if (!after) fail(`no resume token after empty polls (initial=${JSON.stringify(initial)})`);
      if (initial && initial === after) {
        fail(`resume token did not advance across empty getMores: ${after}`);
      }
      console.log('OK');
    } finally {
      await cs.close();
    }
  } finally {
    await client.close();
  }
}

main().catch((err) => fail(`uncaught: ${err && err.stack ? err.stack : err}`));
