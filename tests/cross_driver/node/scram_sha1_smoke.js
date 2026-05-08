// Cross-driver SCRAM-SHA-1 smoke — Node.

const { MongoClient } = require('mongodb');

function fail(msg) {
  console.error(msg);
  process.exit(1);
}

function clientFor(uri, user, pwd, authSource, mech) {
  return new MongoClient(uri, {
    auth: { username: user, password: pwd },
    authSource,
    authMechanism: mech,
    serverSelectionTimeoutMS: 5000,
  });
}

async function main() {
  const uri = process.env.MONGODB_URI;
  const adminPwd = process.env.ADMIN_PASSWORD;
  if (!uri || !adminPwd) fail('MONGODB_URI and ADMIN_PASSWORD required');

  const root = clientFor(uri, 'root', adminPwd, 'admin', 'SCRAM-SHA-256');
  await root.connect();
  try {
    await root.db('admin').command({
      createUser: 'legacy_node',
      pwd: 'pass',
      roles: [],
      mechanisms: ['SCRAM-SHA-1'],
    });
  } finally {
    await root.close();
  }

  const cli = clientFor(uri, 'legacy_node', 'pass', 'admin', 'SCRAM-SHA-1');
  await cli.connect();
  try {
    const status = await cli.db('admin').command({ connectionStatus: 1 });
    const authedUsers = status?.authInfo?.authenticatedUsers ?? [];
    if (
      authedUsers.length === 0 ||
      !authedUsers.find((u) => u.user === 'legacy_node')
    ) {
      fail(`authenticatedUsers: ${JSON.stringify(authedUsers)}`);
    }
    console.log('OK');
  } finally {
    await cli.close();
  }
}

main().catch((err) => fail(`uncaught: ${err && err.stack ? err.stack : err}`));
