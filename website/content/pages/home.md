Title: SecantusDB — the SQLite of document databases
Slug: home
Save_as: index.html
URL:
Status: hidden
Summary: The SQLite of document databases — embeddable, MongoDB-compatible, backed by WiredTiger.

```python
from pymongo import MongoClient
from secantus import SecantusDBServer

# On-disk by default at ./secantus-data;
# pass storage_path=":memory:" for ephemeral.
with SecantusDBServer(port=27017) as server:
    client = MongoClient(server.uri)
    db = client["mydb"]
    db["users"].insert_one({"_id": 1, "name": "Joe"})
    assert db["users"].find_one({"_id": 1})["name"] == "Joe"
```
