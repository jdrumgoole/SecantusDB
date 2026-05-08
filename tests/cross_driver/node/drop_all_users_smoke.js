// Cross-driver dropAllUsersFromDatabase smoke — Node.

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
      .command({
        createUser: 'alice',
        pwd: 'p',
        roles: [{ role: 'read', db: 'shop' }],
      });
    await root
      .db('shop')
      .command({
        createUser: 'bob',
        pwd: 'p',
        roles: [{ role: 'readWrite', db: 'shop' }],
      });
    await root
      .db('other')
      .command({
        createUser: 'carol',
        pwd: 'p',
        roles: [{ role: 'read', db: 'other' }],
      });

    const res = await root.db('shop').command({ dropAllUsersFromDatabase: 1 });
    if (res.n !== 2) fail(`n: got ${res.n}, want 2`);

    const shopUsers = (await root.db('shop').command({ usersInfo: 1 })).users;
    if (shopUsers.length !== 0) fail(`shop users: ${JSON.stringify(shopUsers)}`);

    const otherUsers = (await root.db('other').command({ usersInfo: 1 })).users;
    if (otherUsers.length !== 1 || otherUsers[0].user !== 'carol') {
      fail(`other users: ${JSON.stringify(otherUsers)}`);
    }

    console.log('OK');
  } finally {
    await root.close();
  }
}

main().catch((err) => fail(`uncaught: ${err && err.stack ? err.stack : err}`));
