### secantusdb.com publishes a sitemap

The site had no `sitemap.xml` — `/sitemap.xml` answered 404 — so search engines
had only in-page links to work from. Pelican now renders one from the content it
already knows about: the four product pages, the blog index, and all 62 posts,
each at the same `.html` URL its `rel="canonical"` declares, with `lastmod` taken
from the post date.

Deliberately narrow. The paginated `/blogN.html` listings are duplicates of
content already listed; `/docs/` is excluded because the two Sphinx trees are
grafted into the output *after* Pelican runs, so Pelican cannot enumerate them
and a hand-written guess would rot silently.

#### Added
- `sitemap.xml`, generated at build time from the Pelican content tree.
