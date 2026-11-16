"""Console scripts: hand the process over to the bundled binary."""

from __future__ import annotations

import os
import sys

from secantusdb._server import binary_path


def _exec(name: str) -> None:
    path = str(binary_path(name))
    os.execv(path, [path, *sys.argv[1:]])


def mongodb() -> None:
    _exec("secantusd-mdb")


def postgres() -> None:
    _exec("secantusd-pg")
