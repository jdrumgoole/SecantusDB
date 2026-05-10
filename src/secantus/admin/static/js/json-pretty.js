(function () {
  const TOKEN_RE = /("(?:\\u[a-fA-F0-9]{4}|\\[^u]|[^\\"])*"(?:\s*:)?|\b(?:true|false|null)\b|-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)/g;

  function escapeHtml(s) {
    return s
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;");
  }

  function tokenClass(match) {
    if (match[0] === '"') {
      return match.endsWith(":") ? "json-key" : "json-string";
    }
    if (match === "true" || match === "false") return "json-bool";
    if (match === "null") return "json-null";
    return "json-number";
  }

  // Convert a JSON-as-text string into HTML with tokenised spans.
  // Input is plain text (already indented); output is HTML-safe.
  function prettify(text) {
    if (text == null) return "";
    return escapeHtml(String(text)).replace(TOKEN_RE, (m) => {
      return `<span class="${tokenClass(m)}">${m}</span>`;
    });
  }

  // Walk every <pre class="doc-body"> (and anything tagged .json-pretty)
  // that hasn't already been processed and replace its text content with
  // tokenised HTML.
  function decorate(root) {
    const scope = root || document;
    const nodes = scope.querySelectorAll(
      "pre.doc-body:not([data-json-pretty]), .json-pretty:not([data-json-pretty])",
    );
    nodes.forEach((el) => {
      // Skip empty / whitespace-only or non-JSON-looking content.
      const text = el.textContent;
      if (!text || !text.trim()) return;
      const trimmed = text.trim();
      if (trimmed[0] !== "{" && trimmed[0] !== "[") {
        // Not JSON (e.g. a runCommand string error) — leave alone.
        el.setAttribute("data-json-pretty", "skipped");
        return;
      }
      el.innerHTML = prettify(text);
      el.setAttribute("data-json-pretty", "done");
    });
  }

  // Public helpers — Alpine pages use these for dynamic content.
  window.secantusPrettyJson = prettify;
  window.secantusFormatJsonHtml = function (value) {
    return prettify(JSON.stringify(value, null, 2));
  };
  window.secantusDecorateJson = decorate;

  document.addEventListener("DOMContentLoaded", () => decorate());
  document.addEventListener("htmx:afterSwap", (e) => decorate(e.target));
})();
