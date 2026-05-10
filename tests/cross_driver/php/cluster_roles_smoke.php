<?php
// Cross-driver cluster-role-bundle smoke — PHP.
//
// Provisions a clusterMonitor user and a backup user, verifies
// clusterMonitor can listDatabases but not insert, and that backup
// can read every db but not insert. Both rejection paths return
// code 13 / Unauthorized.

declare(strict_types=1);

require __DIR__ . '/vendor/autoload.php';

use MongoDB\Client;
use MongoDB\Driver\Command;

function bail(string $msg): never {
    fwrite(STDERR, $msg . PHP_EOL);
    exit(1);
}

function authedClient(string $base, string $user, string $pwd, string $authSource): Client {
    $uri = preg_replace(
        '#mongodb://#',
        'mongodb://' . rawurlencode($user) . ':' . rawurlencode($pwd) . '@',
        $base,
        1,
    );
    $sep = (strpos($uri, '?') === false) ? '?' : '&';
    $uri .= $sep . 'authSource=' . $authSource . '&authMechanism=SCRAM-SHA-256';
    return new Client($uri, ['serverSelectionTimeoutMS' => 5000]);
}

$uri = getenv('MONGODB_URI') ?: bail('MONGODB_URI not set');
$adminPwd = getenv('ADMIN_PASSWORD') ?: bail('ADMIN_PASSWORD not set');

$root = authedClient($uri, 'root', $adminPwd, 'admin');
$root->getManager()->executeCommand('admin', new Command([
    'createUser' => 'cluster_mon_php',
    'pwd' => 'p',
    'roles' => [['role' => 'clusterMonitor', 'db' => 'admin']],
]));
$root->getManager()->executeCommand('admin', new Command([
    'createUser' => 'backup_user_php',
    'pwd' => 'p',
    'roles' => [['role' => 'backup', 'db' => 'admin']],
]));
$root->shop->items->insertOne(['_id' => 1, 'name' => 'thing']);

function expectInsertRejected(Client $client, string $label): void {
    $caught = null;
    try {
        $client->shop->items->insertOne(['_id' => 99, 'x' => 1]);
    } catch (\Throwable $e) {
        $caught = $e;
    }
    if ($caught === null) bail("$label insert should have been rejected");
    $code = method_exists($caught, 'getCode') ? $caught->getCode() : 0;
    $msg = $caught->getMessage();
    if ($code !== 13 && strpos($msg, 'Unauthorized') === false) {
        bail("$label insert code=$code message=$msg");
    }
}

// 1. clusterMonitor: listDatabases ok, insert rejected.
$cm = authedClient($uri, 'cluster_mon_php', 'p', 'admin');
$reply = $cm->getManager()->executeCommand('admin', new Command(['listDatabases' => 1]));
$ldb = (array) $reply->toArray()[0];
if ($ldb['ok'] !== 1.0 || ! is_array((array) $ldb['databases'])) {
    bail('clusterMonitor listDatabases: ' . json_encode($ldb));
}
expectInsertRejected($cm, 'clusterMonitor');

// 2. backup: read on every db ok, insert rejected.
$bk = authedClient($uri, 'backup_user_php', 'p', 'admin');
$docs = iterator_to_array($bk->shop->items->find([]));
if (count($docs) !== 1 || (int) ((array) $docs[0])['_id'] !== 1) {
    bail('backup read: ' . json_encode($docs));
}
expectInsertRejected($bk, 'backup');

echo "OK" . PHP_EOL;
