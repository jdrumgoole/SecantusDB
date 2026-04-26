# fongodb

A fake MongoDB server in Python.

FongoDB speaks the subset of the MongoDB wire protocol used by the
[`pymongo`](https://pymongo.readthedocs.io/en/stable/) driver, so test
suites can talk to it instead of starting a real `mongod`. Replica sets,
sharding, and cluster-only features are explicitly out of scope.

## Quick start

Run a server on a fixed port:

```bash
uv run python -m fongodb --host 127.0.0.1 --port 27117
```

Or embed one in a test:

```python
from pymongo import MongoClient
from fongodb import FongoDBServer

with FongoDBServer(port=0) as server:
    client = MongoClient(server.uri)
    # ... run pymongo calls against fongodb ...
```

```{toctree}
:maxdepth: 2
:caption: Contents

api
```
