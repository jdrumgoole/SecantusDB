"""RM-710: the same cases as linux_bucket_catalog*.py against `secantusd-mdb`.

A fresh server per case, so the catalog starts empty as on the Linux
mongod. Usage (from the repo root, with the binary built):

    .venv/bin/python tools/probes/local_bucket_catalog.py path/to/secantusd-mdb

Prints one JSON line per case, in the form the Linux probes print, so the
two outputs diff.
"""

import importlib.util
import json
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time

import pymongo

HERE = pathlib.Path(__file__).resolve().parent


def load(name: str):
    spec = importlib.util.spec_from_file_location(name, HERE / f"{name}.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def start(binary: str, store: str, port: int):
    proc = subprocess.Popen(
        [binary, "--port", str(port), "--storage-path", store],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    deadline = time.time() + 60
    while True:
        try:
            c = pymongo.MongoClient(port=port, serverSelectionTimeoutMS=500)
            c.admin.command("ping")
            c.close()
            return proc
        except Exception:
            if time.time() > deadline:
                proc.kill()
                raise
            time.sleep(0.2)


def main() -> None:
    binary = sys.argv[1]
    only = sys.argv[2:]
    port = 27018
    base = load("linux_bucket_catalog")
    iso = load("linux_bucket_catalog_isolate")
    cases = {
        **{"base:" + k: v for k, v in base.CASES.items()},
        **{"iso:" + k: v for k, v in iso.CASES.items()},
    }
    for name, fn in cases.items():
        if only and not any(o in name for o in only):
            continue
        store = tempfile.mkdtemp(prefix="rm710-")
        proc = start(binary, store, port)
        try:
            c = pymongo.MongoClient(port=port)
            print(json.dumps({"case": name, "got": fn(c)}), flush=True)
            c.close()
        finally:
            proc.terminate()
            proc.wait(timeout=30)
            shutil.rmtree(store, ignore_errors=True)


if __name__ == "__main__":
    main()
