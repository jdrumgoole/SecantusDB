"""Delete the oldest SecantusDB releases from PyPI, through PyPI's own web UI.

PyPI has no API for deleting a release; it is web-only, behind the owner's
login and 2FA. This drives a real, visible browser so the owner logs in
themselves. The profile is kept in ``~/.cache/secantus-pypi-browser``, so
later runs reuse the session. For each release it opens the management page,
types the version into PyPI's confirmation box, submits, and then checks that
PyPI answers 404 for it.

Deleting is PERMANENT: a deleted version number can never be uploaded again.
So the default is a dry run that only lists what would go:

    uv run --no-sync python scripts/pypi_delete_oldest.py              # list the oldest 1
    uv run --no-sync python scripts/pypi_delete_oldest.py --count 3    # list the oldest 3
    uv run --no-sync python scripts/pypi_delete_oldest.py --count 3 --yes
    uv run --no-sync python scripts/pypi_delete_oldest.py --until-under 9.5 --yes

Guard rails:
- The newest release is never touched.
- Only versions matching ``--only-prefix`` (default ``0.5.``) are eligible.
- ``--until-under GB`` stops as soon as the project fits.

Requires Playwright and Chromium (``uv run --no-sync playwright install
chromium`` once).
"""

from __future__ import annotations

import argparse
import json
import random
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

PROJECT = "SecantusDB"
MANAGE = "https://pypi.org/manage/project/secantusdb/release/{version}/"
PROFILE = Path.home() / ".cache" / "secantus-pypi-browser"
UA = "secantusdb-release-tooling (joe@joedrumgoole.com)"


def _get_json(url: str) -> dict:
    # A random query string and no-cache: PyPI's JSON sits behind a CDN that
    # kept reporting deleted releases for a while after they were gone.
    req = urllib.request.Request(
        f"{url}?r={random.random()}", headers={"Cache-Control": "no-cache", "User-Agent": UA}
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.load(resp)


def releases() -> list[tuple[str, str, int]]:
    """``(version, first upload time, bytes)`` for every release that still has
    files, oldest first."""
    out = []
    for version in _get_json(f"https://pypi.org/pypi/{PROJECT}/json")["releases"]:
        try:
            files = _get_json(f"https://pypi.org/pypi/{PROJECT}/{version}/json")["urls"]
        except urllib.error.HTTPError as exc:
            if exc.code == 404:
                continue  # already deleted; the summary is just cached
            raise
        if files:
            uploaded = min(f["upload_time_iso_8601"] for f in files)
            out.append((version, uploaded, sum(f["size"] for f in files)))
    out.sort(key=lambda r: r[1])
    return out


def is_gone(version: str) -> bool:
    try:
        _get_json(f"https://pypi.org/pypi/{PROJECT}/{version}/json")
    except urllib.error.HTTPError as exc:
        return exc.code == 404
    return False


def delete_in_browser(page, version: str, shots: Path) -> None:
    page.goto(MANAGE.format(version=version))
    if "/account/login" in page.url or "/account/two-factor" in page.url:
        print("Log in to PyPI in the browser window (2FA included); waiting up to 5 minutes...")
        page.wait_for_url(lambda u: "/manage/" in u, timeout=300_000)
        page.goto(MANAGE.format(version=version))
    box = page.locator('input[name="confirm_delete_version"]')
    if box.count() == 0:
        shot = shots / f"pypi-delete-{version}.png"
        page.screenshot(path=str(shot), full_page=True)
        raise SystemExit(
            f"Could not find PyPI's delete-confirmation box for {version} "
            f"(the page layout may have changed). Nothing was deleted. Screenshot: {shot}"
        )
    # The box lives in a modal that is hidden until the "Delete" button is
    # pressed; open it the way a person would, then type and submit.
    opener = page.get_by_role("button", name="Delete").or_(page.get_by_role("link", name="Delete"))
    if opener.count():
        opener.first.click()
    box.first.fill(version)
    form = box.first.locator("xpath=ancestor::form[1]")
    form.locator('button[type="submit"], input[type="submit"]').first.click()
    page.wait_for_load_state("networkidle")
    # PyPI may ask the owner to re-enter the password before a destructive
    # action; give them time to do it in the window.
    if "/account/reauthenticate" in page.url or page.locator('input[name="password"]').count():
        print("PyPI asked to confirm your password in the browser window; waiting...")
        page.wait_for_url(lambda u: "/reauthenticate" not in u, timeout=300_000)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--count", type=int, default=1, help="how many of the oldest to delete")
    ap.add_argument("--until-under", type=float, help="stop once the project is under this many GB")
    ap.add_argument("--only-prefix", default="0.5.", help="only versions starting with this")
    ap.add_argument("--yes", action="store_true", help="really delete (default: dry run)")
    args = ap.parse_args()

    rels = releases()
    total = sum(r[2] for r in rels)
    print(f"{PROJECT} on PyPI: {len(rels)} releases with files, {total / 1e9:.2f} GB")
    newest = rels[-1][0] if rels else None
    eligible = [r for r in rels if r[0] != newest and r[0].startswith(args.only_prefix)]

    plan, remaining = [], total
    for version, uploaded, size in eligible:
        if args.until_under is not None:
            if remaining / 1e9 < args.until_under:
                break
        elif len(plan) >= args.count:
            break
        plan.append((version, uploaded, size))
        remaining -= size
    if not plan:
        print("Nothing to delete.")
        return 0
    for version, uploaded, size in plan:
        print(f"  {version:12} uploaded {uploaded[:10]}  {size / 1e6:6.0f} MB")
    print(f"After: {remaining / 1e9:.2f} GB")
    if not args.yes:
        print("Dry run. Re-run with --yes to delete these (permanent).")
        return 0

    from playwright.sync_api import sync_playwright

    PROFILE.mkdir(parents=True, exist_ok=True)
    with sync_playwright() as p:
        ctx = p.chromium.launch_persistent_context(str(PROFILE), headless=False)
        page = ctx.pages[0] if ctx.pages else ctx.new_page()
        for version, _, size in plan:
            print(f"deleting {version} ...", flush=True)
            delete_in_browser(page, version, PROFILE)
            for _ in range(20):
                if is_gone(version):
                    break
                time.sleep(3)
            else:
                ctx.close()
                raise SystemExit(f"{version} still answers on PyPI after the delete; stopping.")
            print(f"  gone ({size / 1e6:.0f} MB freed)")
        ctx.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
