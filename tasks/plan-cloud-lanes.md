# Plan: Linux cloud lanes for the RM backlog

Status: **planned, not started** (2026-11-28). Joe works through Part 1
tomorrow. Part 2 starts once Part 1 is done, by Joe or by a session he points
at this file.

## Why

The MongoDB backlog lane on the Mac is held to two builders (the 10-core
budget and CLAUDE.md's two building agents per session). Both slots are always
in use. On 2026-11-26/27 that gave about 15 fixes a day, and with new items
filed along the way the open count only went from ~118 to 113. Three more lanes
on a Linux x86-64 droplet should add roughly 15-20 fixes a day.

They also run on the reference build. Linux-only failures, like RM-711's
stripe hash and RM-836's lost connections after a stop and continue, then
show up in the lane's own gate instead of on a snapshot PR.

## Decided (Joe, 2026-11-28)

- **Droplet:** `c-16`: 16 dedicated vCPU, 32 GB, 200 GB disk, $0.50/h. lon1,
  with fra1 if lon1 refuses the plan.
- **Claude login:** a `claude setup-token` token (subscription; the droplet's
  sessions share this account's usage limits).
- **Hours:** 12 a day, about $6 a day. A destroy timer enforces it.
- **GitHub:** a fine-grained token scoped to this one repo (recommended,
  not yet confirmed).

## Part 1: Joe

1. **Claude token.**
   - `.env` in the main checkout already has a `CLAUDE_CODE_OAUTH_TOKEN`
     entry. If that is a `setup-token` token you are happy for the droplet to
     use, do nothing.
   - Otherwise, run `claude setup-token` in your own terminal, not through
     `!` in a session, so the token stays out of transcripts. Put it in
     `.env` as `CLOUD_LANE_CLAUDE_TOKEN=...`.
2. **GitHub token.** Create a fine-grained personal access token at
   github.com/settings/personal-access-tokens:
   - Repository access: only `jdrumgoole/SecantusDB`.
   - Permissions: Contents read and write; Pull requests read; Actions read;
     Metadata read.
   - Expiry: 90 days.
   - Put it in `.env` as `CLOUD_LANE_GH_TOKEN=...`.
3. **Check that `main` is protected** against direct pushes, so the token
   cannot write to it: Settings, then Branches, or
   `gh api repos/jdrumgoole/SecantusDB/branches/main/protection`. The lanes
   only ever push to `rm-batch`. Protection is the guard if one misbehaves.
4. **Tell the session to go** (Part 2).

Nothing is created and nothing is billed until step 4.

## Part 2: the session

### 2a. The tool: `tools/cloud_lane.py`
It uses the DigitalOcean API with `DIGITALOCEAN_TOKEN` from `.env`, and the
existing SSH key `~/.ssh/secantus-bench`, which is already registered with
the account. It never prints a token. Its subcommands:

- **`up`**: creates one `c-16` droplet, Ubuntu 24.04, tagged
  `secantus-cloud-lane`, refusing if one with that tag already exists. It
  waits for SSH, records the id and IP in `.scratch/cloud-lane/state.json`,
  and starts the destroy timer.
- **`provision`**: over SSH, as root, then as a `lane` user:
  - the user's home is `/Users/jdrumgoole`, and the repo is at
    `/Users/jdrumgoole/GIT/SecantusDB`, so every absolute path in CLAUDE.md
    and the skills holds;
  - apt: build-essential, cmake, ninja, clang, libclang-dev, pkg-config,
    swig, liblz4-dev, zlib1g-dev, libssl-dev, git, curl, jq;
  - rustup stable with clippy and rustfmt, uv, Python 3.12 through uv, and
    the `gh` CLI;
  - Claude Code (the native installer);
  - mongod 8.2.11 from MongoDB's Ubuntu 24.04 x86-64 tarball, first on
    `PATH`. The version is checked with `mongod --version`;
  - a clone with submodules, `./inv sync`, and one server build as a smoke
    test;
  - the tokens go into `~lane/.lane-env`, mode 600, with git `user.name`
    and `user.email` set as on the Mac.
- **`start`**: runs the restart wrapper (`run_batches.sh`) with
  `fix_many.sh -j 3 -i rm-batch` through `scripts/detached_run.py`, with
  `CARGO_BUILD_JOBS=4` and `SECANTUS_LOAD_CEILING=14`. The droplet's queue is
  its own list (2c).
- **`status`**: the lanes' summary lines, load, disk and the hours left.
- **`down`**: stops the lanes, copies their logs back to
  `.scratch/cloud-lane/`, destroys the droplet, and confirms through the API
  that it is gone.

**The destroy timer.** `up` starts a detached run on the Mac that calls
`down` after 12 hours. Every `status` also checks the droplet's age and
destroys it when it is over 12.5 hours. A powered-off droplet still bills, so
only destroying it ends the charge.

### 2b. Smoke test, before it is trusted
- [ ] `up` and `provision` complete. `mongod --version` shows 8.2.11.
  `claude -p "say ok"` answers, which proves the token works.
  `gh pr list` works and `git push --dry-run origin HEAD:rm-batch` is
  accepted.
- [ ] One full `scripts/gate_rm.py --fast` on a clean `origin/rm-batch`
  worktree exits 0. Its step times are the droplet's baseline.
- [ ] One lane for one hour on one small item: it lands on `rm-batch`, and
  the Mac's lander picks it up in the next snapshot.
- [ ] `down` destroys it, and the API lists no `secantus-cloud-lane` droplet.

### 2c. Splitting the queue between the Mac and the droplet
- The two lanes must never take the same item. Each machine works its own
  list. The droplet takes the items that measure Linux-specific behaviour,
  such as RM-442 (`estimatedSizeBytes` "on Linux") and the
  `only_on("linux")` tests, plus every other item in size order. The Mac
  takes the rest.
- Each list leaves out the other's items. The claim branch each lane pushes
  still catches a collision.
- Landing order is unchanged: everything goes to `rm-batch`, fast-forward
  only, and the Mac's lander takes it to `main` in snapshot PRs.

### 2d. Daily running
- Bring it up each morning with `up`, `provision` (or restore a snapshot,
  below) and `start`. The timer takes it down 12 hours later.
- Once the build has been proven, snapshot the provisioned droplet (about
  $0.06 per GB per month) so the next day's `up` skips the hour of toolchain
  and WiredTiger build. The snapshot is kept until Joe says otherwise.

## Costs
- Droplet: $0.50/h x 12 h = **$6 a day**, about $180 a month at daily use.
- Snapshot: about $1-3 a month.
- **The larger cost is Claude usage.** Three more headless sessions use the
  subscription's limits about 2.5 times as fast as the one Mac lane. Watch
  for lanes stopping on usage limits. The wrapper exits for a human when they
  do, and that is the point at which to cut lanes from 3 to 2.

## Risks and what guards each
- **The droplet is left running:** the 12-hour timer on the Mac, the age
  check in `status`, and the `secantus-cloud-lane` tag so any session can
  list and destroy it.
- **The tokens leak:** they exist only in `.env` on the Mac and in a mode-600
  file on the droplet, and are gone when it is destroyed. The GitHub token
  is scoped to one repo and expires in 90 days. Revoke both if the droplet
  is ever compromised.
- **A bad lane fix:** the same gate as on the Mac, then CI on the snapshot PR
  before anything reaches `main`.
- **The two machines collide:** separate queues and claim branches (2c).

## Not in this plan
- **Keeping all credentials on the Mac.** The sessions would stay here and
  only builds would run on the droplet over SSH. It is safer but takes a few
  days of work on the lane scripts. Revisit if the token-on-droplet setup is
  unwelcome.
- **The PostgreSQL lanes.** The same tool would serve them, but that is the
  postgres session's call.
