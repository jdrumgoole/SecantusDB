### The website lists its talks

#### Added

- A Talks page at `/talks/`, linked from the site's nav and footer, with the
  PyCon Ireland 2026 talk and its slides.

#### Fixed

- A directory URL without its trailing slash, such as
  `/talks/pycon-ie-2026`, `/blog` or `/docs`, redirects to the slash form
  instead of returning 404 (the CloudFront viewer-request function).
