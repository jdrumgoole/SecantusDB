# API reference

The public surface is intentionally tiny. Most use cases need only
`SecantusDBServer` — talk to it through `pymongo` and you get the full
MongoDB-shaped API.

## Server

```{eval-rst}
.. autoclass:: secantus.SecantusDBServer
   :members:
   :show-inheritance:
```

## Storage

`Storage` is the WT-backed document/index store. It's normally accessed
via `pymongo` commands routed through the server, but the public methods
are stable enough to call directly when a test wants explicit control —
notably `prune_ttl`, `explain_plan`, `collection_data_size`, and
`index_sizes`.

```{eval-rst}
.. autoclass:: secantus.storage.Storage
   :members: insert, find_matching, count_matching, update_matching,
             delete_matching, create_index, drop_index, drop_all_indexes,
             list_indexes, list_collections, list_databases,
             collection_exists, drop_collection, drop_database,
             rename_collection, prune_ttl, explain_plan,
             collection_data_size, index_sizes, close
```

## Cursors

```{eval-rst}
.. autoclass:: secantus.cursors.CursorRegistry
   :members: register, next_batch, kill

.. autoclass:: secantus.cursors.CursorNotFound
```

## CLI

```{eval-rst}
.. automodule:: secantus.cli
   :members:
   :undoc-members:
```

## Module-level

```{eval-rst}
.. autodata:: secantus.__version__
```
