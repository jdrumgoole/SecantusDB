// Cross-driver cluster-role-bundle smoke — Node.
//
// Provisions an admin user with the ``clusterMonitor`` role and
// asserts the canonical grant (``listDatabases``) succeeds while a
// non-cluster write (``insertOne``) is rejected with code 13.
// Then a separate user with ``backup`` is verified to read every db
// (including freshly-inserted data) but rejected on writes.

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
      createUser: 'cluster_mon',
      pwd: 'p',
      roles: [{ role: 'clusterMonitor', db: 'admin' }],
    });
    await root.db('admin').command({
      createUser: 'backup_user',
      pwd: 'p',
      roles: [{ role: 'backup', db: 'admin' }],
    });
    // Seed some data the backup user will read.
    await root.db('shop').collection('items').insertOne({ _id: 1, name: 'thing' });
  } finally {
    await root.close();
  }

  // 1. clusterMonitor: listDatabases ok, insert rejected.
  const cm = clientFor(uri, 'cluster_mon', 'p', 'admin');
  await cm.connect();
  try {
    const ldb = await cm.db('admin').command({ listDatabases: 1 });
    if (ldb.ok !== 1 || !Array.isArray(ldb.databases)) {
      fail(`clusterMonitor listDatabases: ${JSON.stringify(ldb)}`);
    }
    let caught = null;
    try {
      await cm.db('shop').collection('items').insertOne({ _id: 99, x: 1 });
    } catch (err) {
      caught = err;
    }
    if (!caught) fail('clusterMonitor insert should have been rejected');
    if (caught.code !== 13) fail(`clusterMonitor insert code=${caught.code}, want 13`);
  } finally {
    await cm.close();
  }

  // 2. backup: read on every db ok, insert rejected.
  const bk = clientFor(uri, 'backup_user', 'p', 'admin');
  await bk.connect();
  try {
    const docs = await bk.db('shop').collection('items').find({}).toArray();
    if (docs.length !== 1 || docs[0]._id !== 1) {
      fail(`backup read: got ${JSON.stringify(docs)}`);
    }
    let caught = null;
    try {
      await bk.db('shop').collection('items').insertOne({ _id: 99, x: 1 });
    } catch (err) {
      caught = err;
    }
    if (!caught) fail('backup insert should have been rejected');
    if (caught.code !== 13) fail(`backup insert code=${caught.code}, want 13`);

    console.log('OK');
  } finally {
    await bk.close();
  }
}

main().catch((err) => fail(`uncaught: ${err && err.stack ? err.stack : err}`));
