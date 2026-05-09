// Cross-driver logical-sessions smoke — Node (mongo-node-driver).
//
// Exercises startSession / endSessions / refreshSessions through the
// raw runCommand surface (the driver's high-level startSession() API
// keeps the lsid private, so we drive the wire commands directly to
// verify the registry state our handlers maintain).

const { MongoClient, Binary } = require('mongodb');

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
    const adminDb = client.db('admin');

    // 1. startSession returns {id: BinData(4, uuid), timeoutMinutes}.
    const started = await adminDb.command({ startSession: 1 });
    if (!started || started.ok !== 1) fail(`startSession failed: ${JSON.stringify(started)}`);
    const wrapped = started.id;
    if (!wrapped || !(wrapped.id instanceof Binary)) {
      fail(`startSession id shape: ${JSON.stringify(started)}`);
    }
    if (wrapped.id.sub_type !== 4) fail(`lsid sub_type: ${wrapped.id.sub_type}`);
    const lsidBytes = wrapped.id.value();
    if (lsidBytes.length !== 16) fail(`lsid length: ${lsidBytes.length}`);
    if (started.timeoutMinutes !== 30) {
      fail(`timeoutMinutes: ${started.timeoutMinutes}, want 30`);
    }

    // 2. endSessions on the freshly-minted lsid → {ok: 1}.
    const ended = await adminDb.command({ endSessions: [wrapped] });
    if (ended.ok !== 1) fail(`endSessions: ${JSON.stringify(ended)}`);

    // 3. refreshSessions implicit-creates an unknown lsid; the server
    // accepts arbitrary 16-byte UUIDs and returns ok.
    const fakeLsid = Buffer.from('0123456789abcdef', 'utf8');
    const fakeWrapped = { id: new Binary(fakeLsid, 4) };
    const refreshed = await adminDb.command({ refreshSessions: [fakeWrapped] });
    if (refreshed.ok !== 1) fail(`refreshSessions: ${JSON.stringify(refreshed)}`);

    console.log('OK');
  } finally {
    await client.close();
  }
}

main().catch((err) => fail(`uncaught: ${err && err.stack ? err.stack : err}`));
