<?php
// Cross-driver bulk-write smoke — PHP (mongo-php-library).
//
// One mixed bulkWrite that insert / update / replace / upsert /
// delete-walks across six docs, then asserts the result counts and
// final collection state.

declare(strict_types=1);

require __DIR__ . '/vendor/autoload.php';

use MongoDB\Client;

function bail(string $msg): never {
    fwrite(STDERR, $msg . PHP_EOL);
    exit(1);
}

$uri = getenv('MONGODB_URI') ?: bail('MONGODB_URI not set');

$client = new Client($uri, ['serverSelectionTimeoutMS' => 5000]);
$coll = $client->bulk_xd_php->c;
$coll->drop();

$coll->insertMany([
    ['_id' => 1, 'kind' => 'old'],
    ['_id' => 2, 'kind' => 'old'],
]);

$res = $coll->bulkWrite([
    ['insertOne' => [['_id' => 3, 'kind' => 'fresh']]],
    ['updateOne' => [['_id' => 1], ['$set' => ['kind' => 'new']]]],
    ['updateMany' => [['kind' => 'old'], ['$set' => ['kind' => 'new']]]],
    ['replaceOne' => [['_id' => 3], ['_id' => 3, 'kind' => 'replaced']]],
    ['updateOne' => [['_id' => 99], ['$set' => ['kind' => 'upserted']], ['upsert' => true]]],
    ['deleteOne' => [['_id' => 2]]],
]);

if ($res->getInsertedCount() !== 1) bail('insertedCount: got ' . $res->getInsertedCount() . ', want 1');
if ($res->getMatchedCount() !== 3) bail('matchedCount: got ' . $res->getMatchedCount() . ', want 3');
if ($res->getModifiedCount() !== 3) bail('modifiedCount: got ' . $res->getModifiedCount() . ', want 3');
if ($res->getUpsertedCount() !== 1) bail('upsertedCount: got ' . $res->getUpsertedCount() . ', want 1');
if ($res->getDeletedCount() !== 1) bail('deletedCount: got ' . $res->getDeletedCount() . ', want 1');

$got = iterator_to_array($coll->find([], ['sort' => ['_id' => 1]]));
if (count($got) !== 3) bail('final docs: got ' . count($got) . ', want 3');
$g0 = (array) $got[0];
$g1 = (array) $got[1];
$g2 = (array) $got[2];
if ((int)$g0['_id'] !== 1 || $g0['kind'] !== 'new') bail('doc[0]: ' . json_encode($g0));
if ((int)$g1['_id'] !== 3 || $g1['kind'] !== 'replaced') bail('doc[1]: ' . json_encode($g1));
if ((int)$g2['_id'] !== 99 || $g2['kind'] !== 'upserted') bail('doc[2]: ' . json_encode($g2));

echo "OK" . PHP_EOL;
