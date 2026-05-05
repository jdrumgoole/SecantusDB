Title: SecantusDB — drop-in MongoDB for single-node Python
Slug: home
Save_as: index.html
URL:
Status: hidden
Summary: SecantusDB is a real MongoDB server written in Python, backed by WiredTiger. It speaks the wire protocol so any standard driver connects unchanged. Use it for tests, dev, embedded apps, and single-node prototypes.

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
