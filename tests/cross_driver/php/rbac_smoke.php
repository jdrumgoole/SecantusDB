<?php
// Cross-driver RBAC smoke — PHP (mongo-php-library).
//
// Provisions a `read`-bound user via the root admin connection, then
// asserts find works and insert is rejected with code 13 / Unauthorized
// when authenticated as the new user.

declare(strict_types=1);

require __DIR__ . '/vendor/autoload.php';

use MongoDB\Client;
use MongoDB\Driver\Exception\CommandException;

function bail(string $msg): never {
    fwrite(STDERR, $msg . PHP_EOL);
    exit(1);
}

$uri = getenv('MONGODB_URI') ?: bail('MONGODB_URI not set');
$adminPwd = getenv('ADMIN_PASSWORD') ?: bail('ADMIN_PASSWORD not set');

$rootUri = preg_replace(
    '#mongodb://#',
    'mongodb://root:' . rawurlencode($adminPwd) . '@',
    $uri,
    1,
);
$root = new Client($rootUri . '?authSource=admin&authMechanism=SCRAM-SHA-256', ['serverSelectionTimeoutMS' => 5000]);

$root->getManager()->executeCommand('shop', new MongoDB\Driver\Command([
    'createUser' => 'viewer_php',
    'pwd' => 'vp',
    'roles' => [['role' => 'read', 'db' => 'shop']],
]));

$viewerUri = preg_replace(
    '#mongodb://#',
    'mongodb://viewer_php:' . rawurlencode('vp') . '@',
    $uri,
    1,
);
$viewer = new Client($viewerUri . '?authSource=shop&authMechanism=SCRAM-SHA-256', ['serverSelectionTimeoutMS' => 5000]);

// Read should succeed.
iterator_to_array($viewer->shop->items->find([]));

$caught = null;
try {
    $viewer->shop->items->insertOne(['x' => 1]);
} catch (CommandException $e) {
    $caught = $e;
} catch (\MongoDB\Driver\Exception\BulkWriteException $e) {
    // insertOne wraps the wire write; the underlying write error
    // surfaces here when the server rejects.
    $caught = $e;
}
if ($caught === null) bail('insert should have been rejected');
$msg = $caught->getMessage();
$code = method_exists($caught, 'getCode') ? $caught->getCode() : 0;
if ($code !== 13 && strpos($msg, 'Unauthorized') === false) {
    bail("unexpected error: code=$code message=$msg");
}

echo "OK" . PHP_EOL;
