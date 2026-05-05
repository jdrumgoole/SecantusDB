// Cross-driver geo smoke test — Node (mongo-node-driver).
//
// Same workload as tests/cross_driver/go/main.go, run through the
// official mongodb npm package. Catches wire-protocol bugs that surface
// only with node-driver's BSON serialization or command shape.
//
// Reads the SecantusDB URI from $MONGODB_URI. Exits 0 on success;
// prints the failure and exits non-zero on any assertion miss.

const { MongoClient } = require('mongodb');

function fail(msg) {
  console.error(msg);
  process.exit(1);
}

function setEq(a, b) {
  if (a.length !== b.length) return false;
  const s = new Set(a);
  for (const x of b) if (!s.has(x)) return false;
  return true;
}

async function main() {
  const uri = process.env.MONGODB_URI;
  if (!uri) fail('MONGODB_URI not set');

  const client = new MongoClient(uri, { serverSelectionTimeoutMS: 5000 });
  await client.connect();
  try {
    const coll = client.db('geo_xdriver').collection('places');
    await coll.drop().catch(() => {});

    const docs = [
      { _id: 1, loc: { type: 'Point', coordinates: [0.0, 0.0] } },
      { _id: 2, loc: { type: 'Point', coordinates: [0.001, 0.0] } },
      { _id: 3, loc: { type: 'Point', coordinates: [50.0, 50.0] } },
    ];
    await coll.insertMany(docs);
    await coll.createIndex({ loc: '2dsphere' });

    // $geoWithin — set comparison since order is unspecified.
    const within = await coll
      .find({ loc: { $geoWithin: { $centerSphere: [[0, 0], 0.001] } } })
      .toArray();
    const ids = within.map((d) => d._id);
    if (!setEq(ids, [1, 2])) fail(`$geoWithin: got ${JSON.stringify(ids)}, want [1, 2]`);

    // $geoNear — ordered by ascending distance, $maxDistance in metres.
    const agg = await coll
      .aggregate([
        {
          $geoNear: {
            near: { type: 'Point', coordinates: [0, 0] },
            distanceField: 'd',
            key: 'loc',
            maxDistance: 200,
          },
        },
      ])
      .toArray();

    const aggIds = agg.map((d) => d._id);
    if (JSON.stringify(aggIds) !== JSON.stringify([1, 2]))
      fail(`$geoNear order: got ${JSON.stringify(aggIds)}, want [1, 2]`);

    if (agg[0].d > 0.001) fail(`$geoNear d[0]: got ${agg[0].d}, want ~0`);
    if (agg[1].d < 100 || agg[1].d > 130)
      fail(`$geoNear d[1]: got ${agg[1].d}, want ~111`);

    console.log('OK');
  } finally {
    await client.close();
  }
}

main().catch((err) => fail(`uncaught: ${err && err.stack ? err.stack : err}`));
