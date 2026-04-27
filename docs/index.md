# secantus

A fake MongoDB server in Python.

SecantusDB speaks the subset of the MongoDB wire protocol used by the
[`pymongo`](https://pymongo.readthedocs.io/en/stable/) driver, so test
suites can talk to it instead of starting a real `mongod`. Replica sets,
sharding, and cluster-only features are explicitly out of scope.

## Quick start

Run a server on a fixed port:

```bash
uv run python -m secantus --host 127.0.0.1 --port 27117
```

Or embed one in a test:

```python
from pymongo import MongoClient
from secantus import SecantusDBServer

with SecantusDBServer(port=0) as server:
    client = MongoClient(server.uri)
    # ... run pymongo calls against secantus ...
```

```{toctree}
:maxdepth: 2
:caption: Contents

api
```
