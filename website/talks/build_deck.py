"""Build a talk deck into one self-contained HTML page for the site.

A deck folder holds ``deck.json`` (title, slide ``order``, font links),
``deck.css`` and ``slides/<id>.html``, one ``<section>`` per slide, each
ending in an optional ``<aside>`` of speaker notes. This writes
``website/content/talks/<deck>/index.html``, which Pelican copies to
``/talks/<deck>/`` as a static file.

    python website/talks/build_deck.py pycon-ie-2026
"""

from __future__ import annotations

import html
import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
OUT_ROOT = HERE.parent / "content" / "talks"

PAGE = """<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
{fonts}
<style>
html,body{{margin:0;height:100%;background:#0B1915;overflow:hidden;}}
#stage{{position:absolute;left:50%;top:50%;width:1920px;height:1080px;transform-origin:0 0;}}
#stage>section{{position:absolute;inset:0;box-sizing:border-box;display:none;}}
#stage>section.on{{display:flex;}}
#stage>section>aside{{display:none;}}
#stage h1,#stage h2,#stage h3,#stage p,#stage ul,#stage ol{{margin:0;}}
#bar{{position:fixed;right:16px;bottom:12px;font:14px/1 system-ui,sans-serif;color:#BFD3CB;opacity:.7;}}
#bar button{{background:none;border:1px solid #BFD3CB;color:inherit;border-radius:4px;padding:4px 10px;margin-left:6px;cursor:pointer;}}
{css}
</style>
</head>
<body>
<div id="stage">
{slides}
</div>
<div id="bar"><span id="num"></span><button id="prev" aria-label="Previous slide">&#8592;</button><button id="next" aria-label="Next slide">&#8594;</button></div>
<script>
(function(){{
  var s=[].slice.call(document.querySelectorAll('#stage>section'));
  var stage=document.getElementById('stage'), i=0;
  function fit(){{var k=Math.min(innerWidth/1920,innerHeight/1080);
    stage.style.transform='scale('+k+') translate(-50%,-50%)';}}
  function show(n){{i=Math.max(0,Math.min(s.length-1,n));
    s.forEach(function(e,j){{e.classList.toggle('on',j===i);}});
    document.getElementById('num').textContent=(i+1)+' / '+s.length;
    history.replaceState(null,'','#'+(i+1));}}
  addEventListener('resize',fit);
  addEventListener('keydown',function(e){{
    if(['ArrowRight','PageDown',' '].indexOf(e.key)>=0){{show(i+1);e.preventDefault();}}
    if(['ArrowLeft','PageUp'].indexOf(e.key)>=0){{show(i-1);e.preventDefault();}}
    if(e.key==='Home')show(0); if(e.key==='End')show(s.length-1);}});
  document.getElementById('next').onclick=function(){{show(i+1);}};
  document.getElementById('prev').onclick=function(){{show(i-1);}};
  fit(); show((parseInt(location.hash.slice(1),10)||1)-1);
}})();
</script>
</body>
</html>
"""

_SECTION = re.compile(r"^\s*<section\b.*</section>\s*$", re.S)


def build(name: str) -> Path:
    src = HERE / name
    deck = json.loads((src / "deck.json").read_text(encoding="utf-8"))
    css = (src / "deck.css").read_text(encoding="utf-8")
    hrefs = sorted({f["href"] for f in deck.get("faces", {}).values() if "href" in f})
    fonts = "\n".join(f'<link rel="stylesheet" href="{html.escape(h)}">' for h in hrefs)
    slides = []
    for sid in deck["order"]:
        text = (src / "slides" / f"{sid}.html").read_text(encoding="utf-8")
        if not _SECTION.match(text):
            raise SystemExit(f"{name}/slides/{sid}.html is not one <section>")
        slides.append(text.strip())
    out = OUT_ROOT / name / "index.html"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(
        PAGE.format(
            title=html.escape(deck["title"]),
            fonts=fonts,
            css=css,
            slides="\n".join(slides),
        ),
        encoding="utf-8",
    )
    return out


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: build_deck.py <deck-folder>")
    print(build(sys.argv[1]))
