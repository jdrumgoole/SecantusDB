<?php
// Cross-driver listDatabases filter smoke — PHP.
//
// Insert one doc into three databases, then list with `name: alpha`
// filter and assert only that one is returned. Re-list with
// `nameOnly: true` and assert at least three dbs.

declare(strict_types=1);

require __DIR__ . '/vendor/autoload.php';

use MongoDB\Client;
use MongoDB\Driver\Command;

function bail(string $msg): never {
    fwrite(STDERR, $msg . PHP_EOL);
    exit(1);
}

$uri = getenv('MONGODB_URI') ?: bail('MONGODB_URI not set');

$client = new Client($uri, ['serverSelectionTimeoutMS' => 5000]);

foreach (['alpha', 'beta', 'gamma'] as $dbName) {
    $client->selectCollection($dbName, 'c')->insertOne(['_id' => 1]);
}

try {
    $reply = $client->getManager()->executeCommand('admin', new Command([
        'listDatabases' => 1,
        'filter' => ['name' => 'alpha'],
    ]));
    $filtered = (array) $reply->toArray()[0];
    $names = array_map(fn($d) => ((array) $d)['name'], (array) $filtered['databases']);
    if ($names !== ['alpha']) {
        bail('filter: got ' . json_encode($names) . ', want [alpha]');
    }

    $reply2 = $client->getManager()->executeCommand('admin', new Command([
        'listDatabases' => 1,
        'nameOnly' => true,
    ]));
    $all = (array) $reply2->toArray()[0];
    $dbs = (array) $all['databases'];
    if (count($dbs) < 3) {
        bail('nameOnly: got ' . count($dbs) . ' dbs, want >= 3');
    }

    echo "OK" . PHP_EOL;
} finally {
    foreach (['alpha', 'beta', 'gamma'] as $dbName) {
        try { $client->dropDatabase($dbName); } catch (\Throwable $e) { /* best-effort */ }
    }
}
