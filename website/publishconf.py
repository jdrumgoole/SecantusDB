"""Production overrides on top of pelicanconf.py.

Used at deploy time: ``pelican content -o output -s publishconf.py``.
"""

from __future__ import annotations

import os
import sys

sys.path.insert(0, os.path.dirname(__file__))

from pelicanconf import *  # noqa: F401,F403,E402

SITEURL = "https://secantusdb.com"
RELATIVE_URLS = False

DELETE_OUTPUT_DIRECTORY = True

FEED_ALL_ATOM = "feed/atom.xml"
CATEGORY_FEED_ATOM = None
TRANSLATION_FEED_ATOM = None
