<?php
// Cross-driver BSON type fidelity smoke — PHP (mongo-php-library /
// ext-mongodb).
//
// Insert one document containing every BSON type that maps to a
// distinct class on the PHP side, then find it back and assert each
// field preserved its class and value. Catches wire-shape divergences
// the way the Node / Go / Java / Rust / Ruby smokes do, but through
// the official PHP driver stack (extension + library).

declare(strict_types=1);

require __DIR__ . '/vendor/autoload.php';

use MongoDB\BSON\Binary;
use MongoDB\BSON\Decimal128;
use MongoDB\BSON\Int64;
use MongoDB\BSON\ObjectId;
use MongoDB\BSON\UTCDateTime;
use MongoDB\Client;

function bail(string $msg): never {
    fwrite(STDERR, $msg . PHP_EOL);
    exit(1);
}

$uri = getenv('MONGODB_URI') ?: bail('MONGODB_URI not set');

$client = new Client($uri, ['serverSelectionTimeoutMS' => 5000]);
$coll = $client->selectCollection('types_xd_php', 'c');
$coll->drop();

$oid = new ObjectId();
$dec = new Decimal128('3.141592653589793238');
// UTCDateTime takes milliseconds since epoch.
$whenMs = 1780000000000;
$when = new UTCDateTime($whenMs);
$bin = new Binary('hello', Binary::TYPE_GENERIC);

// PHP scalar `int` is platform-width — encoded as int64 under the
// driver's default. Force int32 by wrapping ints in a doc literal
// where the BSON type maps from PHP type. The library exposes Int64
// for explicit 64-bit; int32 isn't a separate class (the encoder
// promotes naturally), so the smoke relies on i32 fitting in 32 bits
// which the encoder writes as int32 by default. The doc round-trip
// must show int (regardless of internal int32/int64 tag) on read.
$docIn = [
    '_id' => $oid,
    'i32' => 2147483647,
    'i64' => new Int64(PHP_INT_MAX),
    'f64' => 2.5,
    'dec' => $dec,
    'dt' => $when,
    'bin' => $bin,
    'b' => true,
    'n' => null,
    'sub' => ['x' => 1],
    'arr' => [1, 'two', 3.5],
];
$coll->insertOne($docIn);

$got = $coll->findOne(['_id' => $oid]);
if ($got === null) bail('findOne returned null');

if (! ($got['_id'] instanceof ObjectId) || (string)$got['_id'] !== (string)$oid) {
    bail('_id: got ' . var_export($got['_id'], true));
}
// Both i32 and i64 land back as int (or Int64 wrapper for huge values).
if ((int)$got['i32'] !== 2147483647) bail('i32: got ' . var_export($got['i32'], true));
if ((int)(string)$got['i64'] !== PHP_INT_MAX) bail('i64: got ' . var_export($got['i64'], true));
if (! is_float($got['f64']) || $got['f64'] !== 2.5) bail('f64: got ' . var_export($got['f64'], true));
if (! ($got['dec'] instanceof Decimal128) || (string)$got['dec'] !== '3.141592653589793238') {
    bail('dec: got ' . var_export($got['dec'], true));
}
if (! ($got['dt'] instanceof UTCDateTime) || (int)(string)$got['dt'] !== $whenMs) {
    bail('dt: got ' . var_export($got['dt'], true));
}
if (! ($got['bin'] instanceof Binary) || $got['bin']->getData() !== 'hello' || $got['bin']->getType() !== Binary::TYPE_GENERIC) {
    bail('bin: got ' . var_export($got['bin'], true));
}
if ($got['b'] !== true) bail('b: got ' . var_export($got['b'], true));
if ($got['n'] !== null) bail('n: got ' . var_export($got['n'], true));

// `sub` decodes to MongoDB\Model\BSONDocument by default; convert
// to array for stable access.
$sub = (array)$got['sub'];
if (! isset($sub['x']) || (int)$sub['x'] !== 1) bail('sub.x: got ' . var_export($sub, true));

$arr = (array)$got['arr'];
if (count($arr) !== 3) bail('arr len: got ' . var_export($arr, true));
if ((int)$arr[0] !== 1) bail('arr[0]: got ' . var_export($arr[0], true));
if ($arr[1] !== 'two') bail('arr[1]: got ' . var_export($arr[1], true));
if (! is_float($arr[2]) || $arr[2] !== 3.5) bail('arr[2]: got ' . var_export($arr[2], true));

echo "OK" . PHP_EOL;
