### The droplet benchmark defaults to ams3

DigitalOcean no longer offers the `c-4` server plan in `lon1`, so
`invoke release-benchmark`, `do-bench`, `do-up` and a bare `do-cluster` failed
before creating anything ("Size is not available in this region").

#### Changed

- The default region is `ams3` for the three-droplet cluster; `release-benchmark` takes `--region`. `do-perf` stays on `lon1`, where its `s-8vcpu-16gb` plan is still offered.
- The cluster's VPC name carries its region (`secantus-bench-ams3-vpc`). DigitalOcean VPC names are unique account-wide, so the old region-less name, left in `lon1`, refused a cluster in any other region.
