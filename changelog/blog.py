"""Generate a Pelican blog post from the matching ``docs/changelog.md``
release entry.

The changelog is the system of record (see ``changelog.parse``). This
module reads a release entry by version and emits the marketing-site
blog post — ``website/content/blog/YYYY-MM-DD-release-X-Y-ZbN.md`` —
that the ``secantusdb-website`` skill describes. The two artifacts
share a single source so they don't drift.

Usage::

    uv run --no-sync python -m changelog.blog 0.5.1b17 \\
        --website-dir /Users/jdrumgoole/GIT/SecantusDB-website \\
        --time 16:45:00

If ``--website-dir`` is omitted, the script writes the post to
``website/content/blog/`` relative to whatever directory it's run from
(useful when running inside the SecantusDB-website worktree).

The blog post body is the changelog entry's prose lede verbatim, with a
standard frontmatter prefix and a "GitHub release · PyPI · Tag" link
bar appended. The ``#### Added`` / ``#### Changed`` / ``#### Fixed``
subsections are NOT copied into the blog — they stay in the changelog
as the engineering-reference detail; the lede is the marketing prose.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import sys
from pathlib import Path

from changelog.parse import Release, find_release, parse_changelog

DEFAULT_AUTHOR = "Joe Drumgoole"
DEFAULT_CATEGORY = "Releases"
DEFAULT_TAGS = "release"


def render_blog_post(release: Release, *, time: str = "12:00:00") -> str:
    """Return the markdown text of the blog post for ``release``.

    ``time`` is the ``HH:MM:SS`` portion of the Pelican ``Date:`` header.
    Pelican uses it for ordering within a single day and for the
    feed-item pubDate; pick the release-tag time of day if you have it,
    otherwise the default ``12:00:00`` keeps posts well-ordered.
    """
    if release.is_unreleased:
        raise ValueError("cannot generate a blog post for Unreleased")
    if not release.date:
        raise ValueError(f"release {release.version} has no date")
    if not release.title:
        raise ValueError(f"release {release.version} has no title")
    if not release.lede:
        raise ValueError(f"release {release.version} has no prose lede")
    slug = f"release-{release.version.replace('.', '-')}"
    date = f"{release.date} {time}"
    title = release.title
    body = release.lede
    link_bar = (
        f"[Full release notes on GitHub]"
        f"(https://github.com/jdrumgoole/SecantusDB/releases/tag/{release.tag}) ·\n"
        f"[Install from PyPI](https://pypi.org/project/SecantusDB/{release.version}/) ·\n"
        f"[Tag](https://github.com/jdrumgoole/SecantusDB/tree/{release.tag})"
    )
    return (
        f"Title: {title}\n"
        f"Date: {date}\n"
        f"Slug: {slug}\n"
        f"Author: {DEFAULT_AUTHOR}\n"
        f"Category: {DEFAULT_CATEGORY}\n"
        f"Tags: {DEFAULT_TAGS}\n"
        f"\n"
        f"Summary: {title} (v{release.version}).\n"
        f"\n"
        f"{body}\n"
        f"\n"
        f"{link_bar}\n"
    )


def blog_post_path(release: Release, website_dir: Path) -> Path:
    """Where the blog post lands.

    ``website_dir`` is the root of the SecantusDB-website worktree (the
    directory containing ``website/``). The path mirrors what the
    secantusdb-website skill documents: one file per release under
    ``website/content/blog/YYYY-MM-DD-release-X-Y-ZbN.md``.
    """
    if release.date is None:
        raise ValueError(f"release {release.version} has no date")
    slug = f"release-{release.version.replace('.', '-')}"
    filename = f"{release.date}-{slug}.md"
    return website_dir / "website" / "content" / "blog" / filename


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Generate a Pelican blog post from a SecantusDB changelog entry.",
    )
    parser.add_argument(
        "version",
        help="Version to generate (e.g. 0.5.1b17 or v0.5.1b17).",
    )
    parser.add_argument(
        "--changelog",
        type=Path,
        default=Path(__file__).resolve().parent.parent / "docs" / "changelog.md",
        help="Path to docs/changelog.md (default: project's docs/changelog.md).",
    )
    parser.add_argument(
        "--website-dir",
        type=Path,
        default=None,
        help=(
            "Root of the SecantusDB-website worktree. The post lands at "
            "<website-dir>/website/content/blog/. If omitted, writes to "
            "./website/content/blog/ relative to cwd."
        ),
    )
    parser.add_argument(
        "--time",
        default="12:00:00",
        help="HH:MM:SS suffix for the Pelican Date: header (default 12:00:00).",
    )
    parser.add_argument(
        "--print",
        dest="just_print",
        action="store_true",
        help="Print the rendered post to stdout instead of writing to disk.",
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Overwrite an existing blog post file if one exists.",
    )
    args = parser.parse_args(argv)

    releases = parse_changelog(args.changelog)
    release = find_release(releases, args.version)
    if release is None:
        versions = ", ".join(r.version for r in releases if not r.is_unreleased)
        print(
            f"error: no changelog entry for version {args.version!r}. Known versions: {versions}",
            file=sys.stderr,
        )
        return 2

    try:
        text = render_blog_post(release, time=args.time)
    except ValueError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2

    if args.just_print:
        sys.stdout.write(text)
        return 0

    website_dir = args.website_dir or Path.cwd()
    out_path = blog_post_path(release, website_dir)
    if out_path.exists() and not args.force:
        print(
            f"error: {out_path} already exists. Re-run with --force to overwrite.",
            file=sys.stderr,
        )
        return 3
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(text, encoding="utf-8")
    print(f"wrote {out_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())


# Auto-fill the time portion of the Date: header from the git tag time
# when one is available. Used by the release pipeline to make the blog
# post's timestamp match the tag's. Kept out of ``main()`` so the
# module stays import-clean and testable without a git dep.
def tag_time_iso(repo_root: Path, tag: str) -> str | None:
    """Return ``HH:MM:SS`` of the local-time tag timestamp, or ``None``
    if the tag isn't present in the given repo.
    """
    import subprocess

    try:
        result = subprocess.run(
            ["git", "log", "-1", "--format=%cI", tag],
            cwd=repo_root,
            capture_output=True,
            text=True,
            timeout=10,
        )
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return None
    if result.returncode != 0:
        return None
    iso = result.stdout.strip()
    if not iso:
        return None
    try:
        dt = _dt.datetime.fromisoformat(iso)
    except ValueError:
        return None
    return dt.strftime("%H:%M:%S")
