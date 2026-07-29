"""The admin-UI screenshots stay wired to the docs.

These checks are deliberately browser-free: they run in every CI lane and
cost nothing, and they catch the drift that matters structurally — a page
added to the capture script with no image on disk, an image nothing links
to, a doc reference pointing at a file that doesn't exist.

What they cannot catch is a *stale* image: a PNG of last release's UI is
byte-valid and correctly referenced. Keeping the pictures honest is a
release step (``invoke admin-screenshots``, per the ``secantusdb-release``
skill), not something a test can assert.
"""

from __future__ import annotations

import importlib.util
import re
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
SCRIPT = REPO_ROOT / "scripts" / "admin_screenshots.py"
SHOT_DIR = REPO_ROOT / "docs" / "screenshots"
ADMIN_DOC = REPO_ROOT / "docs" / "admin.md"

# ``![alt](screenshots/admin-foo.png)`` as written in docs/admin.md.
_DOC_IMAGE_RE = re.compile(r"!\[[^\]]*\]\((screenshots/(admin-[a-z0-9-]+\.png))\)")


def _shot_slugs_with_tag(tag: str) -> list[str]:
    """Slugs the capture script marks as reused on a given surface."""
    return [slug for slug, tags in _shots() if tag in tags]


def _shot_slugs() -> list[str]:
    return [slug for slug, _ in _shots()]


def _shots() -> list[tuple[str, tuple[str, ...]]]:
    """Import the capture script and read its declared page list.

    Imported by path rather than ``import scripts.admin_screenshots``:
    ``scripts/`` is not a package and isn't on the path in an installed
    checkout. Module-level code is just constants and dataclasses — no
    server is started by the import.
    """
    spec = importlib.util.spec_from_file_location("admin_screenshots", SCRIPT)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    # Registered before execution because ``@dataclass`` resolves its own
    # module out of ``sys.modules`` while building ``Shot``; without this
    # the import dies in dataclasses.py rather than here.
    sys.modules[spec.name] = module
    try:
        spec.loader.exec_module(module)
        return [(shot.slug, tuple(shot.tags)) for shot in module.SHOTS]
    finally:
        sys.modules.pop(spec.name, None)


@pytest.fixture(scope="module")
def slugs() -> list[str]:
    return _shot_slugs()


def test_every_page_has_a_screenshot(slugs: list[str]) -> None:
    missing = [s for s in slugs if not (SHOT_DIR / f"admin-{s}.png").is_file()]
    assert not missing, (
        f"no screenshot on disk for: {', '.join(missing)}. "
        f"Run `uv run python -m invoke admin-screenshots` to capture them."
    )


def test_every_screenshot_is_referenced(slugs: list[str]) -> None:
    referenced = {m.group(2) for m in _DOC_IMAGE_RE.finditer(ADMIN_DOC.read_text(encoding="utf-8"))}
    unreferenced = sorted(f"admin-{s}.png" for s in slugs if f"admin-{s}.png" not in referenced)
    assert not unreferenced, (
        f"captured but not shown anywhere in docs/admin.md: {', '.join(unreferenced)}. "
        f"Add an image reference in the matching Page tour section."
    )


def test_doc_references_resolve() -> None:
    text = ADMIN_DOC.read_text(encoding="utf-8")
    broken = [
        m.group(1)
        for m in _DOC_IMAGE_RE.finditer(text)
        if not (ADMIN_DOC.parent / m.group(1)).is_file()
    ]
    assert not broken, f"docs/admin.md references missing images: {', '.join(broken)}"


def test_no_orphan_screenshots(slugs: list[str]) -> None:
    """A PNG left behind by a page that no longer exists is dead weight."""
    expected = {f"admin-{s}.png" for s in slugs}
    orphans = sorted(p.name for p in SHOT_DIR.glob("admin-*.png") if p.name not in expected)
    assert not orphans, (
        f"screenshots with no page in SHOTS: {', '.join(orphans)}. "
        f"Delete them, or restore the page they document."
    )


def test_website_screenshots_agree_across_three_places() -> None:
    """Landing-page shots are named identically in all three places.

    The marketing site's copies aren't tracked in git — ``static/img/`` is a
    build-time mirror — so ``website/tasks.py`` copies a named list out of
    ``docs/screenshots/`` at build time. That list, the ``<img>`` tags in the
    theme template, and the ``website`` tags in the capture script all have
    to name the same files. Miss one and the failure is a broken image on
    the live site, which nothing else here would catch.
    """
    theme = REPO_ROOT / "website" / "themes" / "secantus"
    template = (theme / "templates" / "page.html").read_text(encoding="utf-8")
    referenced = set(re.findall(r"img/screenshots/(admin-[a-z0-9-]+\.png)", template))

    website_tasks = (REPO_ROOT / "website" / "tasks.py").read_text(encoding="utf-8")
    block = re.search(r"SITE_SCREENSHOTS = \((.*?)\)", website_tasks, re.DOTALL)
    assert block is not None, "SITE_SCREENSHOTS not found in website/tasks.py"
    copied = set(re.findall(r'"(admin-[a-z0-9-]+\.png)"', block.group(1)))

    tagged = {f"admin-{s}.png" for s in _shot_slugs_with_tag("website")}

    assert referenced == copied == tagged, (
        f"landing-page screenshots disagree — theme template: {sorted(referenced)}, "
        f"website/tasks.py SITE_SCREENSHOTS: {sorted(copied)}, shots tagged 'website': "
        f"{sorted(tagged)}. All three must name the same files."
    )
    on_disk = {n for n in copied if (SHOT_DIR / n).is_file()}
    assert on_disk == copied, (
        f"the website build would fail: {sorted(copied - on_disk)} missing from "
        f"docs/screenshots/. Run `invoke admin-screenshots`."
    )


def test_readme_hero_screenshot_exists() -> None:
    """The README's image links a real file at the path GitHub will serve."""
    readme = (REPO_ROOT / "README.md").read_text(encoding="utf-8")
    linked = set(re.findall(r"docs/screenshots/(admin-[a-z0-9-]+\.png)", readme))
    assert linked, "README no longer shows an admin screenshot"
    missing = sorted(n for n in linked if not (SHOT_DIR / n).is_file())
    assert not missing, f"README links missing screenshot(s): {', '.join(missing)}"
    tagged = {f"admin-{s}.png" for s in _shot_slugs_with_tag("readme")}
    assert linked <= tagged, (
        f"README shows {sorted(linked - tagged)} but those shots aren't tagged "
        f"'readme' in scripts/admin_screenshots.py"
    )


def test_screenshots_are_not_absurdly_large() -> None:
    """Guard the repo against a scale bump nobody reviewed.

    At 1440x900 and a device scale factor of 2 the shots run 100-500 KB.
    A file several times that means someone captured full-page at a higher
    scale, and every release would add that much again to git history.
    """
    too_big = sorted(
        f"{p.name} ({p.stat().st_size / 1_000_000:.1f} MB)"
        for p in SHOT_DIR.glob("admin-*.png")
        if p.stat().st_size > 2_000_000
    )
    assert not too_big, f"unexpectedly large screenshots: {', '.join(too_big)}"
