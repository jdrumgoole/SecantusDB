# Security reports

Nightly output from the scheduled security-review agent
(`.github/workflows/security-review.yml`, driven by
[`scripts/security-review-prompt.md`](../../scripts/security-review-prompt.md)).

Each file is named `YYYY-MM-DD.md` and follows the structure declared in
the prompt: 🔴 CRITICAL / 🟠 WARNING / 🟡 INFO / 🟢 CLEAN findings, plus a
summary block. The review covers **both servers** — the Python server
(`src/secantus/`) and the Rust server (`crates/`) — with the threat model
centred on untrusted wire-protocol parsing, the WiredTiger FFI, the auth
subsystem, the GitHub Actions workflows, and the `secantus` PyPI wheel.

Each report lands on `main` via a short-lived PR that the agent
**squash-merges and deletes in the same run**. A report is docs-only
(`docs/security-reports/**`), which matches the test/wheel workflows'
`paths-ignore`, so it triggers no required CI checks and merges
immediately — no `security-review/*` branch or open PR is ever left
behind. The PR title carries a severity prefix while it's open:

- `[security-critical]` — at least one CRITICAL finding.
- `[security-warning]` — at least one WARNING finding (no CRITICALs).
- `[security-clean]` — clean bill of health.

CRITICAL / WARNING findings that need a code change are tracked as GitHub
Issues (label `security`, title `[security] <summary>`), not as lingering
PRs — merging a report **documents** a finding, it does not fix it. See
[`scripts/security-review-prompt.md`](../../scripts/security-review-prompt.md)
("Land the report") for the exact steps.

The workflow runs nightly at 06:26 UTC and can be triggered on demand from
the Actions tab (`workflow_dispatch`).
