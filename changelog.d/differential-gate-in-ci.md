### The gate that compares us against a real mongod now runs in CI

`tests/test_mongod_differential.py` runs every supported operation against a real `mongod` and asserts an exact match. It gates on `shutil.which("mongod")` — and the CI lanes installed `mongosh` and the Database Tools but no **server**, so it skipped everywhere except whichever dev box happened to run it.

That is a load-bearing gap rather than a cosmetic one: for any change to the error surface, a green CI was **no evidence at all**. The 908-divergence operand campaign that landed immediately before this rewrote error text across ~40 operators, and nothing in CI could have judged a single one of them.

The Linux lanes now install `mongodb-org-server`. **614 tests that were skipping now run.**

#### Changed

- `test`, `test-durable` and `record-durations` install `mongodb-org-server`, and their apt repo moves from **8.0 to 8.2**.

8.2 because that is the series every expectation in the file was measured from (8.2.1 / 8.2.11). The gate compares only the MAJOR, so 8.0 *would* have run — and this file has already been bitten by a **patch**-level difference, an expected-type list whose order changed between 8.2.1 and 8.2.11 and needed `_sort_type_lists`. Pointing CI at a series the expectations were never taken from would manufacture exactly the false failures the file's whole-file skip exists to avoid.

No separate step was needed: the suite's `addopts` excludes `perf` / `online` / `slow` but not `differential`, and the file's `mongod` fixture is module-scoped, so it spawns one server for the whole file.

#### Two things worth knowing, both found by checking rather than assuming

- **There is no `server-8.2.asc` key.** It 404s at both `mongodb.org` and `pgp.mongodb.com`. MongoDB signs both series with one packaging key — verified by fingerprint, not by hope: 8.2's `InRelease` is signed by `4B0752C1BCA238C0B4EE14DC41DE058A4E7DCA05`, which is exactly what `server-8.0.asc` serves. The keyring file is now named `mongodb-server.gpg` rather than `-8.0`, so the two do not have to agree; a plausible-looking "fix" to a versioned 8.2 URL will 404 the job.
- **The patch CI installs is verified, not assumed.** The repo tracks the newest patch (8.2.12 today) while the expectations came from 8.2.11. All 614 cases were run against **both** before landing this, so the version CI actually gets is known-clean. If patch churn ever becomes noise rather than signal, the answer is to pin `mongodb-org-server=8.2.11` — not to loosen the gate.

#### Not changed

macOS and Windows install no server and still skip. macOS runs only on the cron cell, and the Homebrew route for a server is the messier half of an already-awkward tap; Windows has no packaging story worth the flake. The value is in the PR lane, and the PR lane is Linux.
