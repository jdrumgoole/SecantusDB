// Cross-driver change-stream resume smoke — Node (mongo-node-driver).
//
// Same workload as tests/cross_driver/go/cs_resume/main.go: open a
// stream, insert three docs, capture the resume token after the
// first event, reopen with `resumeAfter` and verify events 2 and 3
// arrive. Then reopen with `startAtOperationTime` set to a
// pre-insert timestamp and verify all three events replay.

const { MongoClient } = require('mongodb');

function fail(msg) {
  console.error(msg);
  process.exit(1);
}

function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

// `tryNext()` returns the next event if buffered, or null after one
// non-blocking poll attempt. We loop with a small sleep so the
// awaitData getMore on the server has a chance to drain.
async function nextEvent(stream, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const ev = await stream.tryNext();
    if (ev) return ev;
    await sleep(150);
  }
  fail('timed out waiting for next change event');
}

async function main() {
  const uri = process.env.MONGODB_URI;
  if (!uri) fail('MONGODB_URI not set');

  const client = new MongoClient(uri, { serverSelectionTimeoutMS: 5000 });
  await client.connect();
  try {
    const coll = client.db('cs_resume_xd').collection('c');
    await coll.drop().catch(() => {});

    // Capture lastWrite.opTime BEFORE the inserts so startAtOperationTime
    // resumes from a point earlier than every event we'll produce.
    const hello = await client.db('admin').command({ hello: 1 });
    const startTs = hello.lastWrite?.opTime?.ts;
    if (!startTs) fail(`hello did not include lastWrite.opTime.ts: ${JSON.stringify(hello)}`);

    // 1. Open the stream and drive three inserts. The node-driver
    // doesn't actually create the change-stream cursor on the server
    // until the first iteration request, so we kick `tryNext()` once
    // to pin the resume token before any of the inserts land.
    const cs1 = coll.watch([], { maxAwaitTimeMS: 500 });
    await cs1.tryNext();

    for (const _id of [1, 2, 3]) {
      await coll.insertOne({ _id });
    }
    const e1 = await nextEvent(cs1, 8000);
    if (e1.documentKey._id !== 1) fail(`e1 _id: got ${e1.documentKey._id}, want 1`);
    const resumeAfter = e1._id;
    await cs1.close();

    // 2. Reopen with resumeAfter; expect events 2 then 3.
    const cs2 = coll.watch([], { resumeAfter, maxAwaitTimeMS: 1000 });
    try {
      const e2 = await nextEvent(cs2, 8000);
      const e3 = await nextEvent(cs2, 8000);
      if (e2.documentKey._id !== 2 || e3.documentKey._id !== 3) {
        fail(
          `resumeAfter sequence: e2=${e2.documentKey._id} e3=${e3.documentKey._id}, want 2,3`,
        );
      }
    } finally {
      await cs2.close();
    }

    // 3. Reopen with startAtOperationTime; expect all three events.
    const cs3 = coll.watch([], { startAtOperationTime: startTs, maxAwaitTimeMS: 1000 });
    try {
      const got = [];
      const deadline3 = Date.now() + 8000;
      while (got.length < 3 && Date.now() < deadline3) {
        const ev = await nextEvent(cs3, deadline3 - Date.now());
        got.push(ev.documentKey._id);
      }
      if (got.length !== 3 || got[0] !== 1 || got[1] !== 2 || got[2] !== 3) {
        fail(`startAtOperationTime sequence: ${JSON.stringify(got)}, want [1,2,3]`);
      }
    } finally {
      await cs3.close();
    }

    console.log('OK');
  } finally {
    await client.close();
  }
}

main().catch((err) => fail(`uncaught: ${err && err.stack ? err.stack : err}`));
