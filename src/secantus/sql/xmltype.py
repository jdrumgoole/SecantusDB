"""Postgres ``xml`` type: well-formedness validation, the ``xmlelement`` /
``xmlforest`` constructors, ``xpath`` extraction, and ``xml_is_well_formed``.

Stored as its text (BSON-safe); validated well-formed on cast / coerce. XML
parsing and serialization go through the stdlib ``xml.etree.ElementTree`` (no
external dependency), so the XPath support is a pragmatic subset — absolute
child paths (``/a/b/c``), a trailing ``text()`` node test or ``@attr`` step, and
a leading ``//tag`` descendant search — rather than full XPath 1.0.

Out of scope: DTD / entity expansion (``ElementTree`` disables external entities,
which also side-steps XXE), namespaces in XPath, the ``xmltable`` table function,
``xmlagg`` (an aggregate), and the document/content distinction.
"""

from __future__ import annotations

import re
import xml.etree.ElementTree as ET
from typing import Any


class XmlError(ValueError):
    """A malformed XML literal or an unsupported XPath expression."""


def is_well_formed(text: Any) -> bool:
    """``xml_is_well_formed(text)`` — whether ``text`` parses as a single XML
    element (a well-formed document fragment)."""
    try:
        ET.fromstring(str(text))
        return True
    except ET.ParseError:
        return False


def parse(value: Any) -> str:
    """Validate that ``value`` is well-formed XML and return its text unchanged
    (the canonical stored form). Raises ``XmlError`` if it is not well-formed."""
    s = str(value)
    if not is_well_formed(s):
        raise XmlError(f"invalid XML content: {value!r}")
    return s


def _escape(text: str) -> str:
    return str(text).replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def _escape_attr(text: str) -> str:
    return _escape(text).replace('"', "&quot;")


def element(name: str, attributes: list[tuple[str, Any]], content: list[Any]) -> str:
    """Build ``xmlelement(name, xmlattributes(...), content...)`` — an XML element
    with the given attributes and concatenated (escaped) content."""
    attrs = "".join(f' {k}="{_escape_attr(v)}"' for k, v in attributes if v is not None)
    body = "".join(_escape(c) for c in content if c is not None)
    if body == "":
        return f"<{name}{attrs}/>"
    return f"<{name}{attrs}>{body}</{name}>"


def forest(pairs: list[tuple[str, Any]]) -> str:
    """Build ``xmlforest(value AS name, …)`` — the concatenation of one element per
    pair (a pair whose value is NULL is skipped, as in Postgres)."""
    return "".join(
        f"<{name}>{_escape(value)}</{name}>" for name, value in pairs if value is not None
    )


def concat(*fragments: Any) -> str:
    """``xmlconcat(x1, x2, …)`` — concatenate XML fragments (skipping NULLs)."""
    return "".join(str(f) for f in fragments if f is not None)


_STEP_RE = re.compile(r"[^/]+")


def xpath(path: str, xml_text: Any) -> list[str]:
    """``xpath(expr, xml)`` — evaluate a simple absolute XPath against ``xml`` and
    return the matched nodes as a list of strings (element serialization, text-node
    value for a trailing ``text()``, or the attribute value for a trailing
    ``@attr``). Supports ``/a/b/c`` child paths and a leading ``//tag`` descendant
    search; raises ``XmlError`` for constructs outside that subset."""
    try:
        root = ET.fromstring(str(xml_text))
    except ET.ParseError as exc:
        raise XmlError(f"could not parse XML: {xml_text!r}") from exc

    expr = path.strip()
    # A trailing text() / @attr step is peeled off and applied to the matched nodes.
    want_text = False
    want_attr: str | None = None
    if expr.endswith("/text()"):
        want_text = True
        expr = expr[: -len("/text()")]
    else:
        m = re.search(r"/@([^/]+)$", expr)
        if m is not None:
            want_attr = m.group(1)
            expr = expr[: m.start()]

    if expr.startswith("//"):  # descendant search for the (single) tag
        tag = expr[2:]
        if "/" in tag:
            raise XmlError(f"unsupported XPath: {path!r}")
        matches = list(root.iter(tag))
    else:
        steps = _STEP_RE.findall(expr)
        if not steps:
            raise XmlError(f"unsupported XPath: {path!r}")
        # The first step must name the root element; the rest walk down.
        if steps[0] != root.tag:
            return []
        current = [root]
        for step in steps[1:]:
            nxt: list[ET.Element] = []
            for el in current:
                nxt.extend(el.findall(step))
            current = nxt
        matches = current

    out: list[str] = []
    for el in matches:
        if want_attr is not None:
            val = el.get(want_attr)
            if val is not None:
                out.append(val)
        elif want_text:
            out.append(el.text or "")
        else:
            out.append(ET.tostring(el, encoding="unicode").strip())
    return out
