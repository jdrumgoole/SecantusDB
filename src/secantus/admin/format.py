"""Tiny presentation helpers used by templates and partials."""

from __future__ import annotations

_UNITS = ("B", "KB", "MB", "GB", "TB")


def humanize_bytes(n: int | float | None) -> str:
    """Render a byte count as a short human string (`"4.2 MB"`)."""
    if n is None or n <= 0:
        return "0 B"
    value = float(n)
    for unit in _UNITS:
        if value < 1024 or unit == _UNITS[-1]:
            if unit == "B":
                return f"{int(value)} {unit}"
            return f"{value:.1f} {unit}"
        value /= 1024
    return f"{value:.1f} {_UNITS[-1]}"


def format_count(n: int | float | None) -> str:
    """Comma-separated thousands."""
    if n is None:
        return "0"
    return f"{int(n):,}"


def format_uptime_seconds(seconds: int) -> str:
    if seconds < 60:
        return f"{seconds}s"
    minutes, s = divmod(seconds, 60)
    if minutes < 60:
        return f"{minutes}m {s}s"
    hours, minutes = divmod(minutes, 60)
    if hours < 24:
        return f"{hours}h {minutes}m"
    days, hours = divmod(hours, 24)
    return f"{days}d {hours}h"


__all__ = ["humanize_bytes", "format_count", "format_uptime_seconds"]
