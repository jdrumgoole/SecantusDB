"""Build a talk deck into one self-contained HTML page for the site.

A deck folder holds ``deck.json`` (title, slide ``order``, font links),
``deck.css`` and ``slides/<id>.html``, one ``<section>`` per slide, each
ending in an optional ``<aside>`` of speaker notes. This writes
``website/content/talks/<deck>/index.html``, which Pelican copies to
``/talks/<deck>/`` as a static file.

    python website/talks/build_deck.py pycon-ie-2026

The deck sources in this folder are the canonical copy of each talk; edit
them here and redeploy the site. Below 820px wide (or 500px tall) the page
switches from the projected 16:9 stage to a scrolling reader view.
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
#stage{{position:absolute;left:50%;top:calc(50% - 24px);
  width:1920px;height:1080px;transform-origin:0 0;}}
#stage>section{{position:absolute;inset:0;box-sizing:border-box;display:none;}}
#stage>section.on{{display:flex;}}
#stage>section>aside{{display:none;}}
#stage h1,#stage h2,#stage h3,#stage p,#stage ul,#stage ol{{margin:0;}}
#bar{{position:fixed;right:16px;bottom:10px;color:#BFD3CB;opacity:.7;
  font:14px/1 system-ui,sans-serif;}}
#bar button{{background:none;border:1px solid #BFD3CB;color:inherit;
  border-radius:4px;padding:4px 10px;margin-left:6px;cursor:pointer;}}
{css}
{mobile}
</style>
</head>
<body>
<div id="stage">
{slides}
</div>
<div id="bar"><span id="num"></span>
<button id="prev" aria-label="Previous slide">&#8592;</button>
<button id="next" aria-label="Next slide">&#8594;</button></div>
<script>
(function(){{
  var READER={reader_js};
  var s=[].slice.call(document.querySelectorAll('#stage>section'));
  var stage=document.getElementById('stage'), i=0;
  var reader=matchMedia(READER);
  function fit(){{if(reader.matches){{stage.style.transform='';return;}}
    var k=Math.min(innerWidth/1920,(innerHeight-48)/1080);
    stage.style.transform='scale('+k+') translate(-50%,-50%)';}}
  function show(n){{if(reader.matches)return;
    i=Math.max(0,Math.min(s.length-1,n));
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
  s.forEach(function(e,j){{e.id=e.id||('s'+(j+1));}});
  fit(); show((parseInt(location.hash.slice(1),10)||1)-1);
}})();
</script>
</body>
</html>
"""

#: Phones and small tablets get a scrolling, reflowed reading view instead of
#: the 1920x1080 stage, which at phone width would scale body text to ~6px.
READER_MEDIA = "(max-width: 820px), (max-height: 500px)"

MOBILE_CSS = (
    "@media " + READER_MEDIA + "{"
    "html,body{height:auto;overflow:auto;}"
    "#stage{position:static;width:auto;height:auto;transform:none!important;}"
    "#stage>section{position:relative;display:flex!important;inset:auto;"
    "padding:40px 16px!important;gap:20px!important;min-height:0;}"
    "#bar{display:none;}"
    "#stage h1{font-size:40px!important;}"
    "#stage h2{font-size:28px!important;}"
    "#stage h3{font-size:20px!important;}"
    "#stage p,#stage li,#stage .body,#stage .l{font-size:16px!important;}"
    "#stage .lede{font-size:19px!important;width:auto!important;}"
    "#stage .eyebrow{font-size:12px!important;letter-spacing:2px;}"
    "#stage .big,#stage .stat .n{font-size:40px!important;}"
    "#stage .cards,#stage .stats{grid-template-columns:1fr!important;gap:16px!important;}"
    "#stage .card,#stage .box{padding:16px!important;}"
    "#stage .flow{flex-direction:column;align-items:stretch;gap:8px;}"
    "#stage .arrow{font-size:20px;transform:rotate(90deg);align-self:center;}"
    "#stage .box .t{font-size:17px;}"
    "#stage ul.pts{font-size:16px;padding-left:20px;gap:10px;}"
    "#stage .code{font-size:13px!important;padding:14px!important;"
    "overflow-x:auto;white-space:nowrap!important;}"
    "#stage .foot{position:static;width:auto;font-size:13px;}"
    "#stage .stat{padding-left:14px;border-left-width:4px;}"
    "#stage .rise{animation:none;}"
    "#stage [style*='width:1400px']{width:auto!important;}"
    "}"
)

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
            mobile=MOBILE_CSS,
            reader_js=json.dumps(READER_MEDIA),
            slides="\n".join(slides),
        ),
        encoding="utf-8",
    )
    return out


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: build_deck.py <deck-folder>")
    print(build(sys.argv[1]))
