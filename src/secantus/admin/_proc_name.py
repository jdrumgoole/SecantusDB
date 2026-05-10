"""Best-effort OS-level process-name override.

A user running ``secantusdb-admin`` sees "Python" in the macOS menu
bar, Dock, and Activity Monitor because the launcher process is a
plain Python script. Two cheap fixes get us most of the way there:

* ``setproctitle`` — rewrites argv[0] in ``/proc/.../comm`` and the
  ``ps`` command column. Cross-platform; affects Activity Monitor's
  "command" column on macOS.
* macOS menu bar — mutate the bundle's ``CFBundleName`` *before*
  any AppKit object is created. Pywebview pulls in ``pyobjc`` via
  ``pyobjc-framework-Cocoa`` on macOS, so ``Foundation`` is importable
  in the admin extra.

Both are wrapped in broad try/except — if either dependency isn't
available on the running platform, this becomes a no-op rather than
crashing the launcher.
"""

from __future__ import annotations

import sys


def set_process_name(name: str) -> None:
    """Set the visible process name to ``name`` on every channel we can.

    Idempotent and never raises.
    """
    # ps / top / Activity Monitor command column.
    try:
        import setproctitle  # type: ignore[import-not-found]

        setproctitle.setproctitle(name)
    except Exception:
        pass

    # macOS menu bar / Dock title (when running un-bundled).
    if sys.platform == "darwin":
        try:
            from Foundation import NSBundle  # type: ignore[import-not-found]

            bundle = NSBundle.mainBundle()
            info = bundle.localizedInfoDictionary() or bundle.infoDictionary()
            if info is not None:
                info["CFBundleName"] = name
                info["CFBundleDisplayName"] = name
        except Exception:
            pass


__all__ = ["set_process_name"]
