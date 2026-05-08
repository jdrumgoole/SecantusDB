// Cross-driver custom roles smoke — Node.

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
    await root.db('shop').command({
      createRole: 'shopAuditor',
      privileges: [
        { resource: { db: 'shop', collection: '' }, actions: ['find'] },
      ],
      roles: [],
    });
    await root.db('shop').command({
      createUser: 'auditor_node',
      pwd: 'p',
      roles: [{ role: 'shopAuditor', db: 'shop' }],
    });
  } finally {
    await root.close();
  }

  const auditor = clientFor(uri, 'auditor_node', 'p', 'shop');
  await auditor.connect();
  try {
    await auditor.db('shop').collection('items').find({}).toArray();

    let caught = null;
    try {
      await auditor.db('shop').collection('items').insertOne({ x: 1 });
    } catch (err) {
      caught = err;
    }
    if (!caught) fail('insert should have been rejected for find-only role');
    const message = (caught.errmsg || caught.message || '').toString();
    if (caught.code !== 13 && !message.includes('Unauthorized')) {
      fail(`unexpected error: code=${caught.code} message=${message}`);
    }
  } finally {
    await auditor.close();
  }

  // grantPrivilegesToRole adds insert; reconnect to pick up.
  const root2 = clientFor(uri, 'root', adminPwd, 'admin');
  await root2.connect();
  try {
    await root2.db('shop').command({
      grantPrivilegesToRole: 'shopAuditor',
      privileges: [
        { resource: { db: 'shop', collection: '' }, actions: ['insert'] },
      ],
    });
  } finally {
    await root2.close();
  }

  const auditor2 = clientFor(uri, 'auditor_node', 'p', 'shop');
  await auditor2.connect();
  try {
    await auditor2.db('shop').collection('items').insertOne({ x: 2 });
    console.log('OK');
  } finally {
    await auditor2.close();
  }
}

main().catch((err) => fail(`uncaught: ${err && err.stack ? err.stack : err}`));
