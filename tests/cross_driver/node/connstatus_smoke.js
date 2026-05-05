// Cross-driver connectionStatus smoke — Node (mongo-node-driver).
//
// Mirrors tests/cross_driver/go/connstatus/main.go. Authenticates as
// the bootstrap root user and asserts connectionStatus surfaces the
// expected `authenticatedUsers` + `authenticatedUserRoles` arrays.

const { MongoClient } = require('mongodb');

function fail(msg) {
  console.error(msg);
  process.exit(1);
}

async function main() {
  const uri = process.env.MONGODB_URI;
  const adminPwd = process.env.ADMIN_PASSWORD;
  if (!uri || !adminPwd) fail('MONGODB_URI and ADMIN_PASSWORD required');

  const client = new MongoClient(uri, {
    auth: { username: 'root', password: adminPwd },
    authSource: 'admin',
    authMechanism: 'SCRAM-SHA-256',
    serverSelectionTimeoutMS: 5000,
  });
  await client.connect();
  try {
    const status = await client.db('admin').command({ connectionStatus: 1 });
    const auth = status.authInfo;
    if (!auth) fail(`connectionStatus missing authInfo: ${JSON.stringify(status)}`);
    if (!Array.isArray(auth.authenticatedUsers) || auth.authenticatedUsers.length === 0) {
      fail(`authenticatedUsers empty: ${JSON.stringify(auth)}`);
    }
    if (!Array.isArray(auth.authenticatedUserRoles) || auth.authenticatedUserRoles.length === 0) {
      fail(`authenticatedUserRoles empty: ${JSON.stringify(auth)}`);
    }
    if (auth.authenticatedUserRoles[0].role !== 'root') {
      fail(`expected role=root, got ${JSON.stringify(auth.authenticatedUserRoles[0])}`);
    }
    console.log('OK');
  } finally {
    await client.close();
  }
}

main().catch((err) => fail(`uncaught: ${err && err.stack ? err.stack : err}`));
