### Claude review workflows take Bun from the Actions cache

#### Fixed

- `anthropics/claude-code-action` installs Bun on every run, with setup-bun's
  cache turned off, so every run downloads Bun from GitHub's release
  storage. That download answered HTTP 504 for six minutes and failed
  `claude-review` on PRs #2483 and #2488 before any review ran.
- A new composite action, `.github/actions/cached-bun`, supplies the
  binary, and `claude-code-review.yml`, `claude.yml` and
  `security-review.yml` pass its path as `path_to_bun_executable`, so the
  action skips its own download.
  - It reads the Bun version from the review action's own `action.yml`, so
    it follows when dependabot bumps that action.
  - It restores Bun from the Actions cache. On a miss it downloads Bun with
    retries and a backoff, and checks the zip against the release's
    `SHASUMS256.txt`.
  - It saves the binary straight after the download, so a later failure in
    the job still leaves it cached.
- The nightly security review and `@claude` runs are on the default branch,
  so they save the copy every PR restores. A pull request cannot write to
  that copy.
