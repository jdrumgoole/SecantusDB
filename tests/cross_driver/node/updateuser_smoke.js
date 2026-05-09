// Cross-driver updateUser smoke — Node (mongo-node-driver).
//
// Mirrors tests/cross_driver/go/updateuser/main.go. Provisions a user
// with `orig`, rotates to `rotated` via updateUser, and asserts the
// old password no longer authenticates while the new one does.

const { MongoClient } = require('mongodb');

function fail(msg) {
  console.error(msg);
  process.exit(1);
}

function clientFor(uri, user, pwd, authSource) {
  return new MongoClient(uri, {
    auth: { username: user, password: pwd },
    authSource,
    authMechanism: 'SCRAM-SHA-256',
    serverSelectionTimeoutMS: 5000,
  });
}

async function main() {
  const uri = process.env.MONGODB_URI;
  const adminPwd = process.env.ADMIN_PASSWORD;
  if (!uri || !adminPwd) fail('MONGODB_URI and ADMIN_PASSWORD required');

  const root = clientFor(uri, 'root', adminPwd, 'admin');
  await root.connect();
  try {
    await root.db('admin').command({
      createUser: 'alice_xd',
      pwd: 'orig',
      roles: [{ role: 'read', db: 'admin' }],
    });
    await root.db('admin').command({ updateUser: 'alice_xd', pwd: 'rotated' });
  } finally {
    await root.close();
  }

  // Old password — must fail.
  let oldErr = null;
  const oldClient = clientFor(uri, 'alice_xd', 'orig', 'admin');
  try {
    await oldClient.connect();
    await oldClient.db('admin').command({ ping: 1 });
  } catch (err) {
    oldErr = err;
  } finally {
    await oldClient.close().catch(() => {});
  }
  if (!oldErr) fail('old password should not authenticate');

  // New password — must succeed.
  const newClient = clientFor(uri, 'alice_xd', 'rotated', 'admin');
  try {
    await newClient.connect();
    await newClient.db('admin').command({ ping: 1 });
  } finally {
    await newClient.close().catch(() => {});
  }

  console.log('OK');
}

main().catch((err) => fail(`uncaught: ${err && err.stack ? err.stack : err}`));
