// Cross-driver RBAC smoke — Node (mongo-node-driver).
//
// Same shape as tests/cross_driver/go/rbac/main.go. Provisions a
// `read`-bound user via the root admin connection, then asserts find
// works and insert is rejected with `Unauthorized` when authenticated
// as the new user. The node driver's MongoServerError surfaces the
// errmsg / code from the reply doc — anything other than 13 trips here.

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
    await root
      .db('shop')
      .command({ createUser: 'viewer', pwd: 'vp', roles: [{ role: 'read', db: 'shop' }] });
  } finally {
    await root.close();
  }

  const viewer = clientFor(uri, 'viewer', 'vp', 'shop');
  await viewer.connect();
  try {
    const items = viewer.db('shop').collection('items');
    await items.find({}).toArray();

    let caught = null;
    try {
      await items.insertOne({ x: 1 });
    } catch (err) {
      caught = err;
    }
    if (!caught) fail('insert should have been rejected');
    const message = (caught.errmsg || caught.message || '').toString();
    if (caught.code !== 13 && !message.includes('Unauthorized')) {
      fail(`unexpected error: code=${caught.code} message=${message}`);
    }
    console.log('OK');
  } finally {
    await viewer.close();
  }
}

main().catch((err) => fail(`uncaught: ${err && err.stack ? err.stack : err}`));
