### CI stops cancelling its own answers

Two blind spots on the same theme — checks that quietly produced no result,
so a breakage could sit on `main` looking green.

The docs had no CI at all. `test.yml` and both wheel workflows carry
`paths-ignore: ['**.md', 'LICENSE*', 'docs/**']`, which is right for a test
matrix but means a docs-only commit skips CI entirely — and that is exactly
how the Sphinx build came to be failing on `main` through many green pushes.
A new `Docs` workflow builds both trees with warnings-as-errors. It has no
`paths` filter on purpose: `conf.py` runs autodoc over the package, so a
malformed docstring in a code-only commit can break the docs build, and
filtering to `docs/**` would recreate the same blind spot facing the other
way. The build compiles nothing — the WiredTiger extension is mocked — so
running it on everything is the cheapest job in the repo.

The second was subtler. All three of `Tests`, `Build wheels` and `Build
secantus-core wheels` cancelled in-progress runs per ref. On a feature branch
that is what you want, since the newest push should win. On `main` every
merge lands on the same ref, so on a busy day each merge cancelled the
previous commit's post-merge run: several consecutive merges each showed
`cancelled`, meaning the default branch went long stretches with no completed
result and a wheel-only regression would not have surfaced until a release
tag. Cancellation is now disabled on `main` only, so those runs queue and
each finishes, while pull requests keep newest-push-wins.

#### Added

- A `Docs` workflow building `docs/` and `docs-rust/` with `sphinx-build -W`,
  covering the gap left by every other workflow's `paths-ignore`.

#### Fixed

- `Tests`, `Build wheels` and `Build secantus-core wheels` no longer cancel
  their own post-merge runs on `main`. Runs on the default branch queue and
  complete; pull-request branches still cancel superseded runs.
