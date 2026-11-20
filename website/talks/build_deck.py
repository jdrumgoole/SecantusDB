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
import subprocess
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
<link rel="icon" type="image/svg+xml" href="/theme/img/favicon.svg">
{fonts}
<style>
html,body{{margin:0;height:100%;background:#020617;overflow:hidden;}}
#stage{{position:absolute;left:50%;top:calc(50% - 24px);
  width:1920px;height:1080px;transform-origin:0 0;}}
#stage>section{{position:absolute;inset:0;box-sizing:border-box;display:none;}}
#stage>section.on{{display:flex;}}
#stage>section>aside{{display:none;}}
#stage h1,#stage h2,#stage h3,#stage p,#stage ul,#stage ol{{margin:0;}}
#bar{{position:fixed;right:16px;bottom:10px;color:#cbd5e1;opacity:.7;
  font:14px/1 system-ui,sans-serif;}}
.nfoot{{position:absolute;left:40px;bottom:24px;display:flex;gap:16px;align-items:baseline;
  font:500 18px/1 system-ui,sans-serif;}}
.nlink{{color:inherit;opacity:.35;text-decoration:none;border-bottom:1px dotted currentColor;}}
.nver{{opacity:.35;font-variant-numeric:tabular-nums;}}
.nlink:hover,.nlink:focus{{opacity:.8;}}
#notes{{position:fixed;inset:0;background:rgba(11,25,21,.72);z-index:10;display:flex;
  align-items:center;justify-content:center;padding:16px;box-sizing:border-box;}}
#notes[hidden]{{display:none;}}
#notes .nbox{{position:relative;background:#f8fafc;color:#0f172a;max-width:760px;width:100%;
  max-height:86vh;overflow:auto;border-radius:8px;padding:32px 36px;box-sizing:border-box;
  font:17px/1.6 'Inter',system-ui,sans-serif;}}
#notes h2{{font:600 24px/1.25 'Inter',system-ui,sans-serif;margin:0 0 16px;}}
#notes .nkick{{font:600 12px/1 system-ui,sans-serif;letter-spacing:2px;text-transform:uppercase;
  color:#0e7490;margin:0 0 8px;}}
#notes p{{margin:0 0 12px;font-size:17px!important;line-height:1.6!important;
  color:#0f172a!important;width:auto!important;}}
#notes ul{{margin:0 0 12px;padding-left:20px;}}
#notes .time{{color:#475569;font-size:14px;}}
#nclose{{position:absolute;right:12px;top:10px;background:none;border:0;font-size:28px;
  cursor:pointer;color:#475569;}}
#bar button{{background:none;border:1px solid #cbd5e1;color:inherit;
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
<div id="notes" role="dialog" aria-modal="true" aria-labelledby="notes-h" hidden>
<div class="nbox"><button id="nclose" aria-label="Close speaker notes">&#215;</button>
<p class="nkick">Speaker notes</p><h2 id="notes-h"></h2><div id="nbody"></div></div></div>
<script>
(function(){{
  var READER={reader_js}, VERSION={version_js};
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
    if(!notes.hidden){{if(e.key==='Escape'||e.key==='n')closeNotes();return;}}
    if(e.key==='n'&&!reader.matches){{openNotes(s[i]);return;}}
    if(['ArrowRight','PageDown',' '].indexOf(e.key)>=0){{show(i+1);e.preventDefault();}}
    if(['ArrowLeft','PageUp'].indexOf(e.key)>=0){{show(i-1);e.preventDefault();}}
    if(e.key==='Home')show(0); if(e.key==='End')show(s.length-1);}});
  document.getElementById('next').onclick=function(){{show(i+1);}};
  document.getElementById('prev').onclick=function(){{show(i-1);}};
  s.forEach(function(e,j){{e.id=e.id||('s'+(j+1));}});
  var notes=document.getElementById('notes'),last=null;
  function openNotes(sec){{var a=sec.querySelector('aside');if(!a)return;
    var h=sec.querySelector('h1,h2,.big');
    document.getElementById('notes-h').textContent=h?h.textContent:'';
    document.getElementById('nbody').innerHTML=a.innerHTML;
    last=document.activeElement;notes.hidden=false;document.getElementById('nclose').focus();}}
  function closeNotes(){{notes.hidden=true;if(last)last.focus();}}
  s.forEach(function(e){{var f=document.createElement('div');f.className='nfoot';
    if(e.querySelector('aside')){{
      var l=document.createElement('a');l.href='#';l.className='nlink';
      l.textContent='Speaker notes';
      l.onclick=function(ev){{ev.preventDefault();openNotes(e);}};f.appendChild(l);}}
    var v=document.createElement('span');v.className='nver';v.textContent=VERSION;f.appendChild(v);
    e.appendChild(f);}});
  document.getElementById('nclose').onclick=closeNotes;
  notes.onclick=function(ev){{if(ev.target===notes)closeNotes();}};
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
    "#stage .nfoot{position:static;align-self:flex-start;font-size:14px;}"
    "#stage .nlink,#stage .nver{opacity:.6;}"
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


def deck_version(src: Path, base: str) -> str:
    """The deck's version, ``v<major>.<minor>.<patch>``.

    ``major.minor`` is ``deck.json``'s ``version``, set by hand for a
    milestone. ``patch`` counts the commits that changed the deck's folder
    since that version string was introduced, so every committed change
    gets a new number without anyone bumping it. ``+local`` marks
    uncommitted edits; outside git the patch is omitted.
    """

    def git(*args: str) -> str:
        return subprocess.run(
            ["git", "-C", str(src), *args], capture_output=True, text=True, check=True
        ).stdout.strip()

    pattern = r'"version"\s*:\s*"' + re.escape(base) + '"'
    try:
        since = git("log", "-1", "--format=%H", "--pickaxe-regex", "-S", pattern, "--", "deck.json")
        patch = int(git("rev-list", "--count", f"{since}..HEAD", "--", ".")) if since else 0
        dirty = git("status", "--porcelain", "--", ".")
    except (OSError, ValueError, subprocess.CalledProcessError):
        return f"v{base}"
    return f"v{base}.{patch}" + ("+local" if dirty else "")


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
            version_js=json.dumps(deck_version(src, deck["version"])),
            slides="\n".join(slides),
        ),
        encoding="utf-8",
    )
    return out


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: build_deck.py <deck-folder>")
    print(build(sys.argv[1]))
