# Examples

Each example below runs end-to-end against an embedded `SecantusDBServer`.
The server starts on an OS-assigned port (`port=0`) and uses an in-memory
WiredTiger store, so you can copy any snippet into a `python` session or
test file and it will work without external setup.

The running theme is a small wine-cellar app — bottles, regions, vintages —
that exercises the operations a real test suite tends to drive.

## Connect

```python
from pymongo import MongoClient
from secantus import SecantusDBServer

with SecantusDBServer(port=0) as server:
    client = MongoClient(server.uri)
    db = client["wine_cellar"]
    print(db.command("ping"))   # {'ok': 1.0}
```

`server.uri` is a `mongodb://127.0.0.1:<port>` string — pass it straight
to `MongoClient`. The context manager handles the listener thread, the
WiredTiger cleanup, and the `:memory:` temp directory.

## Insert

```python
from pymongo import MongoClient
from secantus import SecantusDBServer

with SecantusDBServer(port=0) as server:
    bottles = MongoClient(server.uri)["wine_cellar"]["bottles"]

    one = bottles.insert_one(
        {"name": "Pommard 2018", "region": "Burgundy", "year": 2018}
    )
    print(one.inserted_id)      # auto-generated ObjectId

    many = bottles.insert_many(
        [
            {"_id": 2, "name": "Brunello 2015", "region": "Tuscany", "year": 2015},
            {"_id": 3, "name": "Barolo 2017", "region": "Piedmont", "year": 2017},
            {"_id": 4, "name": "Pommard 2020", "region": "Burgundy", "year": 2020},
        ]
    )
    print(many.inserted_ids)    # [2, 3, 4]

    print(bottles.count_documents({}))   # 4
```

`insert_one` returns auto-generated ObjectIds; explicit `_id` values are
preserved as-is, including non-string types (int, Decimal128, ObjectId,
UUID, BSON binary).

## Indexes

SecantusDB has a real query planner backed by WiredTiger B-trees. Indexes
accelerate equality, range, sort, and `$in` queries; `explain()` reports
which index was used.

```python
from pymongo import MongoClient
from secantus import SecantusDBServer

with SecantusDBServer(port=0) as server:
    bottles = MongoClient(server.uri)["wine_cellar"]["bottles"]

    # Single-field ascending.
    bottles.create_index([("year", 1)])

    # Compound, mixed direction. The planner can also serve a sort on
    # `region` ascending alone using this index's leading field.
    bottles.create_index([("region", 1), ("year", -1)])

    # Partial index — only docs that match the filter are entered.
    # Useful for keeping the index small when you only query a subset.
    bottles.create_index(
        [("year", 1)],
        name="year_recent",
        partialFilterExpression={"year": {"$gte": 2015}},
    )

    # Unique constraint.
    bottles.create_index([("name", 1)], unique=True)

    # See what's there.
    for ix in bottles.list_indexes():
        print(ix["name"], ix.get("key"))

    # Drop a single index.
    bottles.drop_index("year_recent")
```

For TTL indexes (`expireAfterSeconds`) and the full picker semantics, see
[Indexes](indexes.md).

## Query

```python
from pymongo import MongoClient
from secantus import SecantusDBServer

with SecantusDBServer(port=0) as server:
    bottles = MongoClient(server.uri)["wine_cellar"]["bottles"]
    bottles.insert_many(
        [
            {"_id": 1, "name": "Pommard 2018", "region": "Burgundy", "year": 2018},
            {"_id": 2, "name": "Brunello 2015", "region": "Tuscany", "year": 2015},
            {"_id": 3, "name": "Barolo 2017", "region": "Piedmont", "year": 2017},
            {"_id": 4, "name": "Pommard 2020", "region": "Burgundy", "year": 2020},
        ]
    )
    bottles.create_index([("year", 1)])

    # Equality.
    burgundies = list(bottles.find({"region": "Burgundy"}))

    # Range, with sort + projection.
    drinking_window = list(
        bottles.find(
            {"year": {"$gte": 2015, "$lte": 2018}},
            projection={"name": 1, "year": 1, "_id": 0},
        ).sort("year")
    )

    # $in.
    italian = list(bottles.find({"region": {"$in": ["Tuscany", "Piedmont"]}}))

    # Aggregation pipeline.
    by_region = list(
        bottles.aggregate(
            [
                {"$group": {
                    "_id": "$region",
                    "count": {"$sum": 1},
                    "oldest": {"$min": "$year"},
                }},
                {"$sort": {"_id": 1}},
            ]
        )
    )

    # explain() shows which index the planner picked (IXSCAN vs COLLSCAN).
    plan = bottles.find({"year": {"$gte": 2017}}).explain()
    stage = plan["queryPlanner"]["winningPlan"]["inputStage"]
    print(stage["stage"], stage.get("indexName"))   # IXSCAN year_1
```

## Update

```python
from pymongo import MongoClient
from secantus import SecantusDBServer

with SecantusDBServer(port=0) as server:
    bottles = MongoClient(server.uri)["wine_cellar"]["bottles"]
    bottles.insert_one({"_id": 1, "name": "Pommard 2018", "drunk": False})

    bottles.update_one({"_id": 1}, {"$set": {"drunk": True, "score": 92}})
    bottles.update_many({"region": "Burgundy"}, {"$inc": {"score": 1}})

    # Atomic findAndModify.
    doc = bottles.find_one_and_update(
        {"_id": 1},
        {"$set": {"notes": "Black cherry, earth"}},
        return_document=True,   # ReturnDocument.AFTER
    )
```

## Drop

```python
from pymongo import MongoClient
from secantus import SecantusDBServer

with SecantusDBServer(port=0) as server:
    client = MongoClient(server.uri)
    cellar = client["wine_cellar"]
    cellar["bottles"].insert_one({"name": "Pommard 2018"})
    cellar["tastings"].insert_one({"bottle": "Pommard 2018", "score": 92})

    # Drop one collection — indexes go with it.
    cellar["tastings"].drop()
    print(cellar.list_collection_names())     # ['bottles']

    # Drop the whole database.
    client.drop_database("wine_cellar")
    print("wine_cellar" in client.list_database_names())   # False
```

`drop_database` removes every collection, every index, and the database
metadata in one call. After it returns, the next `client["wine_cellar"]`
reference creates a fresh, empty database.

## Pytest fixture

Wrap the server in a fixture to get a fresh database per test. With
`pytest-xdist` each worker gets its own port, so the suite can fan out
freely.

```python
import pytest
from pymongo import MongoClient
from secantus import SecantusDBServer


@pytest.fixture
def db():
    with SecantusDBServer(port=0) as server:
        client = MongoClient(server.uri)
        try:
            yield client["wine_cellar"]
        finally:
            client.close()


def test_insert_round_trip(db):
    db["bottles"].insert_one({"_id": 1, "name": "Pommard 2018"})
    assert db["bottles"].find_one({"_id": 1})["name"] == "Pommard 2018"


def test_index_picks_correctly(db):
    db["bottles"].create_index([("year", 1)])
    db["bottles"].insert_many(
        [{"_id": i, "year": 2010 + i} for i in range(10)]
    )
    plan = db["bottles"].find({"year": {"$gte": 2015}}).explain()
    stage = plan["queryPlanner"]["winningPlan"]["inputStage"]
    assert stage["stage"] == "IXSCAN"
    assert stage["indexName"] == "year_1"
```

## Persistent storage

All examples above use `:memory:` storage (the default). For a persistent
WiredTiger home — useful for ad-hoc inspection or cross-process scenarios:

```python
from secantus import SecantusDBServer

server = SecantusDBServer(host="127.0.0.1", port=27117, db_home="/tmp/cellar-data")
server.start()
try:
    # ... server is now persistent at /tmp/cellar-data ...
    pass
finally:
    server.stop()
```

Reopening the same `db_home` later restores all collections, indexes, and
documents.
