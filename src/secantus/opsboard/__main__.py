"""``python -m secantus.opsboard`` → the CLI."""

from __future__ import annotations

import sys

from secantus.opsboard.cli import main

if __name__ == "__main__":
    sys.exit(main())
