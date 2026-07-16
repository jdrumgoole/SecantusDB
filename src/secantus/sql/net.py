"""Postgres network address types: ``inet`` / ``cidr`` / ``macaddr``.

Values are stored as canonical text (``inet`` / ``cidr`` as ``addr/masklen``,
``macaddr`` as ``xx:xx:xx:xx:xx:xx``) and parsed with Python's ``ipaddress`` at
operator-evaluation time. This module normalises, renders, and compares them;
``secantus.sql.scalar`` / ``typemap`` / ``planner`` wire it into the SQL surface.

Out of scope: the ``<<=`` / ``>>=`` operators (sqlglot can't parse them), ``inet``
arithmetic (``+`` / ``-`` an int), ``macaddr8``, and GiST network indexes.
"""

from __future__ import annotations

import ipaddress
import re
from typing import Any


class NetError(ValueError):
    """A malformed network-address literal."""


def normalize_inet(text: str) -> str:
    """Normalise an ``inet`` literal to ``addr/masklen`` (default /32 or /128).
    Host bits are preserved (``inet`` keeps the specific address)."""
    s = str(text).strip()
    try:
        iface = ipaddress.ip_interface(s)
    except ValueError as e:
        raise NetError(f"invalid inet value: {text!r}") from e
    return f"{iface.ip}/{iface.network.prefixlen}"


def normalize_cidr(text: str) -> str:
    """Normalise a ``cidr`` literal to the canonical ``network/masklen`` (host bits
    must be zero — ``strict``)."""
    s = str(text).strip()
    try:
        net = ipaddress.ip_network(s, strict=True)
    except ValueError as e:
        raise NetError(f"invalid cidr value: {text!r}") from e
    return f"{net.network_address}/{net.prefixlen}"


def normalize_macaddr(text: str) -> str:
    """Normalise a ``macaddr`` to the canonical lower-case colon form."""
    hexes = re.findall(r"[0-9A-Fa-f]{2}", str(text))
    if len(hexes) != 6:
        raise NetError(f"invalid macaddr value: {text!r}")
    return ":".join(h.lower() for h in hexes)


def render_inet(value: Any) -> str:
    """Render an ``inet`` — drop the mask when it's a full host (/32 or /128)."""
    s = str(value)
    iface = ipaddress.ip_interface(s)
    full = iface.max_prefixlen
    return (
        str(iface.ip)
        if iface.network.prefixlen == full
        else f"{iface.ip}/{iface.network.prefixlen}"
    )


def render_cidr(value: Any) -> str:
    return str(value)


def _network_of(value: Any) -> ipaddress._BaseNetwork:
    """The network an ``inet`` / ``cidr`` value denotes (host bits masked off)."""
    return ipaddress.ip_interface(str(value)).network


def contains(a: Any, b: Any) -> bool:
    """Does network ``a`` contain (or equal, for the same net) ``b``? ``a >> b``."""
    na, nb = _network_of(a), _network_of(b)
    if na.version != nb.version:
        return False
    return nb.subnet_of(na)


def overlaps(a: Any, b: Any) -> bool:
    """Do ``a`` and ``b`` overlap (either contains the other)? ``a && b``."""
    na, nb = _network_of(a), _network_of(b)
    if na.version != nb.version:
        return False
    return na.overlaps(nb)


def host(value: Any) -> str:
    """``host(inet)`` — the address with no mask."""
    return str(ipaddress.ip_interface(str(value)).ip)


def masklen(value: Any) -> int:
    """``masklen(inet)`` — the netmask length."""
    return ipaddress.ip_interface(str(value)).network.prefixlen


def network(value: Any) -> str:
    """``network(inet)`` — the network part as ``cidr`` text (``addr/masklen``)."""
    net = _network_of(value)
    return f"{net.network_address}/{net.prefixlen}"


def netmask(value: Any) -> str:
    """``netmask(inet)`` — the netmask as an address."""
    return str(_network_of(value).netmask)


def broadcast(value: Any) -> str:
    """``broadcast(inet)`` — the broadcast address, with the value's mask."""
    net = _network_of(value)
    return f"{net.broadcast_address}/{net.prefixlen}"


def abbrev(value: Any, *, is_cidr: bool) -> str:
    """``abbrev`` — the abbreviated text (cidr drops a full-host mask)."""
    net = _network_of(value)
    if is_cidr and net.prefixlen == net.max_prefixlen:
        return str(net.network_address)
    return (
        render_cidr(normalize_cidr(f"{net.network_address}/{net.prefixlen}"))
        if is_cidr
        else render_inet(value)
    )


def family(value: Any) -> int:
    """``family(inet)`` — 4 or 6."""
    return _network_of(value).version
