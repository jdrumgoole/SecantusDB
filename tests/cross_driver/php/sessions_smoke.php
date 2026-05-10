<?php
// Cross-driver logical-sessions smoke — PHP.
//
// Drives startSession / endSessions / refreshSessions through raw
// runCommand so the wire-level lsid round-trip is what's exercised.

declare(strict_types=1);

require __DIR__ . '/vendor/autoload.php';

use MongoDB\BSON\Binary;
use MongoDB\Client;
use MongoDB\Driver\Command;

function bail(string $msg): never {
    fwrite(STDERR, $msg . PHP_EOL);
    exit(1);
}

$uri = getenv('MONGODB_URI') ?: bail('MONGODB_URI not set');

$client = new Client($uri, ['serverSelectionTimeoutMS' => 5000]);
$mgr = $client->getManager();

// 1. startSession returns {id: BinData(4, uuid), timeoutMinutes}.
$reply = $mgr->executeCommand('admin', new Command(['startSession' => 1]));
$started = (array) $reply->toArray()[0];
if ($started['ok'] !== 1.0) bail('startSession: ' . json_encode($started));
$wrapped = (array) $started['id'];
$inner = $wrapped['id'] ?? null;
if (! ($inner instanceof Binary) || $inner->getType() !== 4 || strlen($inner->getData()) !== 16) {
    bail('lsid shape: ' . var_export($inner, true));
}
if (($started['timeoutMinutes'] ?? 0) !== 30) {
    bail('timeoutMinutes: ' . var_export($started['timeoutMinutes'] ?? null, true));
}

// 2. endSessions on the new lsid → ok.
$reply = $mgr->executeCommand('admin', new Command([
    'endSessions' => [['id' => $inner]],
]));
$ended = (array) $reply->toArray()[0];
if ($ended['ok'] !== 1.0) bail('endSessions: ' . json_encode($ended));

// 3. refreshSessions implicit-creates an unknown lsid; server accepts.
$fake = new Binary('0123456789abcdef', Binary::TYPE_UUID);
$reply = $mgr->executeCommand('admin', new Command([
    'refreshSessions' => [['id' => $fake]],
]));
$refreshed = (array) $reply->toArray()[0];
if ($refreshed['ok'] !== 1.0) bail('refreshSessions: ' . json_encode($refreshed));

echo "OK" . PHP_EOL;
