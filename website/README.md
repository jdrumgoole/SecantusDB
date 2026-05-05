# SecantusDB marketing site

Pelican-based static site for [secantusdb.com](https://secantusdb.com), hosted on
S3 + CloudFront. Source is dev-only — excluded from the SecantusDB sdist/wheel via
`pyproject.toml`'s `sdist.exclude`.

## Layout

```
website/
├── pelicanconf.py            # dev settings (relative URLs, no feeds)
├── publishconf.py            # prod overrides (https://secantusdb.com, feeds on)
├── tasks.py                  # invoke tasks (build / serve / clean / deploy / infra-up)
├── content/
│   ├── pages/home.md         # claims index.html
│   ├── pages/404.md          # CloudFront error page
│   └── blog/*.md
├── themes/secantus/          # custom theme
│   ├── templates/            # base, index, article, page, partials
│   └── static/css/site.css   # tokens lifted from brandkit/brand.html
└── infra/
    ├── aws.py                # boto3 idempotent provisioner + deployer
    └── aws-state.json        # generated (gitignored): bucket / dist id / cert arn
```

The brandkit lives at `../brandkit/` and is copied into
`themes/secantus/static/img/` at build time (the `assets` task) — single source of
truth: edit SVGs in `brandkit/`, never in the theme.

The version shown in the alpha banner and footer is read from `pyproject.toml`
at build time, so a release bump propagates without touching the site source.

## Local preview

All commands are run **from the `website/` directory** (the website's invoke tasks
are local to that dir, not wired into the top-level `tasks.py`).

```bash
uv sync --extra dev --extra website
cd website
uv run python -m invoke serve   # http://localhost:8000, autoreload
```

Other tasks:

```bash
uv run python -m invoke build           # dev build into website/output/
uv run python -m invoke build --prod    # prod build (absolute URLs, feeds)
uv run python -m invoke clean           # wipe output + copied assets
uv run python -m invoke assets          # mirror brandkit/*.svg into theme
```

## Deployment

### Prerequisites

1. **AWS credentials** available via the standard chain (env vars / `~/.aws/credentials`
   / SSO / instance profile). The IAM principal needs S3, CloudFront, ACM, Route 53,
   and `sts:GetCallerIdentity` permissions.
2. **Route 53 public hosted zone** for `secantusdb.com` already created (the
   provisioner refuses to create zones — they cost money and are typically created
   during domain registration).

### One-time infrastructure bootstrap

```bash
cd website
uv run python -m invoke infra-up
```

Provisions, idempotently:

- Private S3 bucket `secantusdb.com` (public access fully blocked).
- ACM cert in `us-east-1` for `secantusdb.com` + `www.secantusdb.com`, validated via
  DNS records written to the existing Route 53 zone.
- CloudFront distribution + Origin Access Control fronting the bucket. HTTPS-only,
  HTTP/2 + HTTP/3, gzip + brotli compression, custom 403/404 → `/404.html`.
- S3 bucket policy granting only the OAC `s3:GetObject`.
- Route 53 A + AAAA aliases for apex + `www` pointing at the distribution.

State is written to `infra/aws-state.json` (gitignored). Subsequent runs short-circuit.
First-time setup takes ~10–15 minutes — most of it is CloudFront's initial deploy.

### Publishing changes

```bash
cd website
uv run python -m invoke deploy
```

Runs in order:

1. Production Pelican build (clean — `DELETE_OUTPUT_DIRECTORY = True`).
2. Upload to S3 with cache-control headers:
   - `*.css *.js *.svg *.png *.jpg *.jpeg *.webp *.woff *.woff2 *.ico` →
     `public, max-age=31536000, immutable`
   - everything else (HTML, XML feeds, etc.) →
     `public, max-age=300, must-revalidate`
3. Delete remote keys not present locally (mirrors `aws s3 sync --delete`).
4. CloudFront `/*` invalidation.

Typical end-to-end runtime: under a minute.

### Tear-down

`infra-down` is a stub. The site is small but the resources have entanglements
(certificate must be detached before delete, distribution must be disabled before
delete, etc.). Tear-down is a manual operation via the AWS console — intentional
guard rail.

## Conventions

- **The brandkit owns the SVGs.** The build copies them in; never edit
  `themes/secantus/static/img/`.
- **Don't hardcode the version.** Read it from `pyproject.toml` (the
  `pelicanconf.py` injection makes `SECANTUS_VERSION` available in templates).
- **Cache-busting strategy.** HTML/feeds get a 5-minute TTL; everything else gets a
  year + `immutable`. CloudFront `/*` invalidation runs on every deploy as
  belt-and-braces — cheap (1000 free invalidation paths per month, `/*` counts as 1).
- **No GitHub Actions for the website (yet).** Deploys are manual via `invoke`.
  Adding a workflow later just wraps `invoke deploy` with the IAM role
  scoped to the site's resources.
