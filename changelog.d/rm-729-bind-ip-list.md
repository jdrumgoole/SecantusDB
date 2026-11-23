### `--bind_ip` takes a list of addresses

`secantusd-mdb --bind_ip 127.0.0.1,10.0.0.5` binds every address in the
list, as mongod 8.2 does, instead of refusing it. Without `--ipv6`, which
mongod needs to resolve IPv6 and this server does not take, an IPv6 address
in a list is passed over with mongod's "Found no addresses for peer"
warning (id 23021), and the rest are served.

#### Fixed

- `--bind_ip` with more than one address binds a listener per address.
  Fixes RM-729 (tests/backlog/test_rm_found_2026_11_20_bind_ip.py::test_a_bind_ip_list_serves_as_mongod_serves_it)
