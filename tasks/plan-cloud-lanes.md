# Plan: Linux cloud lanes for the RM backlog

Status: **planned, not started** (2026-11-28; revised the same day after a
review, below). Joe works through Part 1. Part 2 starts once Part 1 is done,
by Joe or by a session he points at this file.

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

- **Droplet:** `c-16`: 16 dedicated vCPU, 32 GB, 200 GB disk, $0.50/h
  ($336 a month list). lon1, with fra1 if lon1 refuses the plan.
- **Claude login:** a `claude setup-token` token (subscription; the droplet's
  sessions share this account's usage limits).
- **Hours:** 12 a day, about $6 a day. A destroy timer enforces it.

## What the review found (2026-11-28)

These were checked against the repo and GitHub, and the plan below is changed
to answer each one.

- **`main` is not protected.** `gh api .../branches/main/protection`
  answers 404 "Branch not protected". The only ruleset (18530430, "Protect
  release tags") covers `refs/tags/v*` and `refs/tags/secantusdb-v*`, and
  blocks only deleting and rewriting them (`deletion`, `non_fast_forward`).
  It has no bypass actors.
- **A token with Contents write could start a release.** Creating a
  `secantusdb-v*` or `secantusd-pg-v*` tag runs `release-binaries.yml`,
  `release-pg-binaries.yml`, `publish-crates.yml` or `publish-pypi.yml`.
  Tag creation is not blocked, and `secantusd-pg-v*` tags are not covered
  at all. A fine-grained token cannot be limited to some branches or tags.
- **On the droplet, a token of Joe's is Joe.** He is the repo's admin, so a
  ruleset he can bypass, the droplet can bypass too. The droplet needs an
  identity of its own: a deploy key for git, and a read-only token for
  `gh`. The rulesets then give only the admin role a bypass.
- **mongod version.** CI installs `mongodb-org-server` from MongoDB's
  `noble/mongodb-org/8.2` apt repository (`test.yml`), which is 8.2.12
  today, not 8.2.11. The droplet's gate has to measure the same mongod as
  the snapshot PR's CI.
- **What the lanes need that is only on the Mac:**
  - the restart wrapper `run_batches.sh` (in `.scratch/rm-run/`, not in
    the repo);
  - the lane's skip list `.scratch/rm-run/declined.txt`, which also holds
    the items subagents are working;
  - in `~/.claude`: the `ci-check` skill (the other skills fix-one uses are
    in the repo's `.claude/skills`), the global `CLAUDE.md`,
    `agents/runner.md`, and `settings.json`, whose allow rules the headless
    `--permission-mode auto` sessions rely on.
- **The lander follows the Mac lane only.** `land_batch.sh rm-batch <pid>`
  stops when that one batch process exits, even while the droplet is still
  pushing to `rm-batch`.
- **The existing `CLAUDE_CODE_OAUTH_TOKEN` in `.env`** may be the one the
  Claude Code Review job uses. The droplet gets a token of its own, so
  revoking one leaves the other working.
- **The destroy timer was a single point of failure.** A `detached_run` on
  the Mac dies with a sleeping Mac, a dropped network or a `close-session`
  sweep, and the age check in `status` runs only when someone runs
  `status`.
- **Unmeasured: memory for three lanes.** Three gates at once, each with a
  mongod, pytest workers and a debug link, may not fit in 32 GB.
- **Already fine:** `gate_rm.py` puts the Homebrew mongod 8.2.11 first on
  `PATH` only when that directory exists, so it runs unchanged on Linux.
  The SSH key `~/.ssh/secantus-bench` is present.

## Part 1: Joe

1. **A deploy key for the droplet's git.** It is how the droplet pushes. It
   is not an admin, so the rulesets in steps 2 and 3 apply to it.
   - The session can make the key pair for you
     (`ssh-keygen -t ed25519 -N '' -f .scratch/cloud-lane/deploy_key`, in
     the main checkout's ignored `.scratch/`), or you can make it yourself.
   - Add the public half under Settings, then Deploy keys, then Add deploy
     key. Title: `secantus-cloud-lane`. Tick **Allow write access**.
2. **A ruleset on `main`.** Settings, then Rules, then Rulesets, then New
   branch ruleset:
   - Name: `Protect main`. Enforcement: Active.
   - Target: the default branch.
   - Bypass list: **Repository admin** only.
   - Rules: Restrict deletions; Block force pushes; Require a pull request
     before merging (0 approvals is fine).
   - **Check this first:** confirm no release step pushes straight to
     `main` (the `secantusdb-release` skill). With an admin bypass your own
     pushes still work, but anything pushing as the deploy key or as a
     GitHub Actions token is refused.
3. **Widen the tag ruleset** (18530430, "Protect release tags"):
   - Add `refs/tags/secantusd-pg-v*` to its targets.
   - Add the rule **Restrict creations**.
   - Add **Repository admin** to its bypass list, so your own release tags
     still go through. Today it has no bypass, which blocks your deletes
     as intended. With the bypass, the deletion and rewrite rules no longer
     apply to you. If you want to keep that, make this a second ruleset (the
     same three patterns, Restrict creations, admin bypass) and leave
     18530430 as it is.
   - A release that tags from a workflow (`GITHUB_TOKEN`) would now be
     refused. Check that the release tags are pushed from your own checkout,
     as the release skill describes.
4. **A read-only GitHub token for `gh`**, at
   github.com/settings/personal-access-tokens (fine-grained):
   - Repository access: only `jdrumgoole/SecantusDB`.
   - Permissions: Contents read; Pull requests read; Actions read; Metadata
     read. No write permission of any kind.
   - Expiry: 90 days.
   - Put it in `.env` as `CLOUD_LANE_GH_TOKEN=...`. The repo is public, so
     this token exposes little if it leaks. It is there so `gh` is not held
     to 60 unauthenticated API calls an hour.
5. **A Claude token of the droplet's own.** Run `claude setup-token` in your
   own terminal, not through `!` in a session, so the token stays out of
   transcripts. Put it in `.env` as `CLOUD_LANE_CLAUDE_TOKEN=...`. Do not
   reuse `CLAUDE_CODE_OAUTH_TOKEN`.
6. **Tell the session to go** (Part 2).

Nothing is created in the cloud and nothing is billed until step 6.

## Part 2: the session

### 2a. Before anything is created: what the droplet must get from the Mac
- **Commit the restart wrapper** as `scripts/run_batches.sh` (it is
  `.scratch/rm-run/run_batches.sh` today), through `rm-batch`, so both
  machines run the same script.
- **Commit the queue split** (2d) as a file in the repo, say
  `tasks/lane-queues.md`, naming each machine's items. Each machine's lane
  reads its own section, and both keep their own skip list
  (`declined.txt`) for items they declined. A subagent's item still goes
  in the Mac's skip list; the droplet sees it as the subagent's pushed claim
  branch, which carries the id (`rm-826-plan-cache`).
- **Make the lander follow both machines.** `land_batch.sh` takes a batch
  pid today. It gains a mode that keeps landing while `rm-batch` keeps
  moving (say, until `rm-batch` has not moved for two hours and no lane is
  running on either machine), so it does not stop when the Mac lane does.

### 2b. The tool: `tools/cloud_lane.py`
It uses the DigitalOcean API with `DIGITALOCEAN_TOKEN` from `.env`, and the
existing SSH key `~/.ssh/secantus-bench`, which is already registered with
the account. It never prints a token. It only ever lists, creates or destroys
droplets with the `secantus-cloud-lane` tag, and never touches another
droplet on the account. Its subcommands:

- **`up`**: creates one `c-16` droplet, Ubuntu 24.04, tagged
  `secantus-cloud-lane`, refusing if one with that tag already exists. It
  waits for SSH, records the id and IP in `.scratch/cloud-lane/state.json`,
  and loads the destroy timer (2c).
- **`provision`**: over SSH, as root, then as a `lane` user:
  - the user's home is `/Users/jdrumgoole`, and the repo is at
    `/Users/jdrumgoole/GIT/SecantusDB`, so every absolute path in CLAUDE.md
    and the skills holds;
  - apt: build-essential, cmake, ninja-build, clang, libclang-dev,
    pkg-config, swig, liblz4-dev, zlib1g-dev, libssl-dev, git, curl, jq;
  - **mongod from the same source CI uses:** MongoDB's
    `noble/mongodb-org/8.2` apt repository and the `mongodb-org-server`
    package, as in `test.yml`. Record `mongod --version` in `state.json`,
    and each morning check it against the version CI's latest run
    installed. If the two differ, stop and say so; do not start the lanes;
  - rustup stable with clippy and rustfmt, uv, Python 3.12 through uv, and
    the `gh` CLI;
  - Claude Code (the native installer);
  - **from the Mac's `~/.claude`:** `skills/ci-check/`, `CLAUDE.md`,
    `agents/runner.md` and `settings.json`, copied as they are (no
    credentials are in them; check that before copying);
  - **git over the deploy key:** the private key at `~lane/.ssh/deploy_key`
    (mode 600), an SSH config entry for `github.com` that uses it, and the
    clone's `origin` set to `git@github.com:jdrumgoole/SecantusDB.git`;
  - a clone with submodules, `./inv sync`, and one server build as a smoke
    test;
  - `CLOUD_LANE_CLAUDE_TOKEN` (as `CLAUDE_CODE_OAUTH_TOKEN`) and
    `CLOUD_LANE_GH_TOKEN` (as `GH_TOKEN`) in `~lane/.lane-env`, mode 600;
    git `user.name` and `user.email` set as on the Mac.
- **`start`**: runs `scripts/run_batches.sh` with `fix_many.sh -j N -i
  rm-batch` through `scripts/detached_run.py`, with `CARGO_BUILD_JOBS=4`
  and `SECANTUS_LOAD_CEILING=14`. N is what the smoke test (2e) measured:
  2, or 3 if three gates fit. Its queue is the droplet's section of the
  queue file.
- **`status`**: the lanes' summary lines, load, memory, disk, and the hours
  left.
- **`down`**: stops the lanes, copies their logs back to
  `.scratch/cloud-lane/`, destroys the droplet, and confirms through the API
  that no `secantus-cloud-lane` droplet is left.

### 2c. The destroy timer
A powered-off droplet still bills, so only destroying it ends the charge.
- **A launchd job on the Mac**, not a `detached_run`, so it survives the
  session ending and a `close-session` sweep. `up` loads it, set to run
  `cloud_lane.py down` 12 hours later; `down` unloads it. If the Mac was
  asleep at that time, launchd runs the job when it wakes.
- **`status` checks the age too**, and destroys the droplet when it is over
  12.5 hours old.
- **Optional, Joe's call:** a scheduled GitHub Actions job, every 30
  minutes, that destroys any `secantus-cloud-lane` droplet older than 13
  hours. It is the only guard that works with the Mac off, but it puts a
  DigitalOcean token in the repo's secrets. If it is wanted, use a custom
  scoped token (droplet read and delete only), not the full one in `.env`.

### 2d. Splitting the queue between the Mac and the droplet
- The two lanes must never take the same item. Each machine works its own
  list, from the committed queue file (2a). The droplet takes the items that
  measure Linux-specific behaviour, such as RM-442 (`estimatedSizeBytes`
  "on Linux") and the `only_on("linux")` tests, plus every other item in
  size order. The Mac takes the rest.
- The claim branch each lane pushes still catches a collision, and fix-one
  checks it before it starts.
- Landing order is unchanged: everything goes to `rm-batch`, fast-forward
  only, and the Mac's lander takes it to `main` in snapshot PRs.

### 2e. Smoke test, before it is trusted
- [ ] `up` and `provision` complete. `mongod --version` matches CI's.
  `claude -p "say ok"` answers, which proves the token works.
  `gh pr list` works.
- [ ] **The guards hold.** From the droplet:
  - `git push --dry-run origin HEAD:rm-batch` is accepted;
  - read both rulesets back with
    `gh api repos/jdrumgoole/SecantusDB/rulesets/<id>`: each is active,
    targets what step 2 or 3 says, and its bypass list holds Repository
    admin and nothing else.
  - **Do not test the guards by pushing to `main` or creating a release
    tag.** If either rule were missing, that push would be the very thing
    the rule is there to stop.
- [ ] One full `scripts/gate_rm.py --fast` on a clean `origin/rm-batch`
  worktree exits 0. Its step times and peak memory are the droplet's
  baseline.
- [ ] **Memory:** two gates at once, then three, on two clean worktrees,
  with peak memory recorded (`free -m` sampled every 10 s). The lane count
  for `start` is the largest that stays under about 28 GB with no OOM kill
  in `dmesg`.
- [ ] One lane for one hour on one small item: it lands on `rm-batch`, and
  the Mac's lander picks it up in the next snapshot.
- [ ] `down` destroys it, the launchd job is unloaded, and the API lists no
  `secantus-cloud-lane` droplet.

### 2f. Daily running
- Bring it up each morning with `up`, `provision` (or restore a snapshot,
  below) and `start`. The timer takes it down 12 hours later.
- Once the build has been proven, snapshot the provisioned droplet (about
  $0.06 per GB per month) so the next day's `up` skips the hour of toolchain
  and WiredTiger build. A restored snapshot still runs the morning mongod
  version check and `apt upgrade` of `mongodb-org-server` (2b). The
  snapshot is kept until Joe says otherwise.

## Costs
- Droplet: $0.50/h x 12 h = **$6 a day**, about $180 a month at daily use.
- Snapshot: about $1-3 a month.
- **The larger cost is Claude usage.** Two or three more headless sessions
  use the subscription's limits two to three times as fast as the one Mac
  lane. Watch for lanes stopping on usage limits. The wrapper exits for a
  human when they do, and that is the point at which to cut a lane.

## Risks and what guards each
- **The droplet is left running:** the launchd timer on the Mac, the age
  check in `status`, optionally the scheduled Actions job, and the
  `secantus-cloud-lane` tag, so any session can list and destroy it.
- **A lane pushes to `main` or creates a release tag:** the droplet pushes
  with a deploy key that is not on either ruleset's bypass list, and its
  `gh` token is read-only.
- **The credentials leak:**
  - The deploy key can push any branch except `main` and create no release
    tag. Remove it under Deploy keys.
  - The `gh` token is read-only, scoped to one repo, and expires in 90 days.
  - The Claude token is the droplet's own; revoke it without touching CI's.
  - All three exist only in `.env` on the Mac and in mode-600 files on the
    droplet, and are gone when it is destroyed. Revoke all three if the
    droplet is ever compromised.
- **A bad lane fix:** the same gate as on the Mac, against the same mongod
  as CI, then CI on the snapshot PR before anything reaches `main`.
- **The two machines collide:** separate committed queues and claim
  branches (2d).
- **A lane deletes another session's branch:** the deploy key can delete
  any branch except `main`. fix-one deletes only the claim branch it
  pushed. If that ever proves untrue, add a ruleset restricting deletions
  of all branches except `rm-*` claim branches, with admin bypass.

## Not in this plan
- **Keeping all credentials on the Mac.** The sessions would stay here and
  only builds would run on the droplet over SSH. It is safer but takes a few
  days of work on the lane scripts. Revisit if the deploy key and tokens on
  the droplet are unwelcome.
- **The PostgreSQL lanes.** The same tool would serve them, but that is the
  postgres session's call.
