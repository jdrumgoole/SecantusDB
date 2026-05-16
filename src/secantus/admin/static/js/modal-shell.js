// Shared scaffolding for the admin UI's confirmation / edit modals.
//
// Modals are HTMX-loaded into ``#modal`` and unloaded by setting that
// element's ``innerHTML`` back to ``""``. This file gives every modal
// the same three a11y wins:
//
// 1. Escape closes (overlay listens for ``keydown.escape``).
// 2. Focus returns to the element that opened the modal.
// 3. Tab / Shift+Tab cycle within the modal's focusable children
//    rather than escaping back into the page behind.
//
// Templates wire in by calling ``closeModal()`` instead of clearing
// ``#modal`` manually and by setting ``x-init="setupModal($el)"`` on
// the overlay div.

(function () {
  // The trigger that opened the current modal, so we can restore focus
  // to it on close. Captured on every htmx swap *into* ``#modal``
  // because Alpine ``x-init`` runs after the swap (too late to read
  // ``document.activeElement``, which by then is the body).
  let lastTrigger = null;

  document.addEventListener("htmx:beforeSwap", (evt) => {
    if (evt.target && evt.target.id === "modal") {
      lastTrigger = document.activeElement;
    }
  });

  function focusableWithin(root) {
    return Array.from(
      root.querySelectorAll(
        'button:not([disabled]), [href], input:not([disabled]):not([type="hidden"]),' +
          ' select:not([disabled]), textarea:not([disabled]),' +
          ' [tabindex]:not([tabindex="-1"])',
      ),
    );
  }

  // Public: open a modal by fetching ``url`` and dropping the response
  // into ``#modal``. For callers that need to bypass htmx attributes on
  // the trigger (e.g. a form that synthesises a URL from input values
  // at submit time). Captures the trigger so close-restore-focus works.
  window.openModal = async function openModal(url) {
    lastTrigger = document.activeElement;
    const resp = await fetch(url, { headers: { "HX-Request": "true" } });
    const html = await resp.text();
    const m = document.getElementById("modal");
    if (!m) return;
    m.innerHTML = html;
    if (window.htmx) window.htmx.process(m);
  };

  // Public: clear ``#modal`` and restore focus to the trigger element.
  window.closeModal = function closeModal() {
    const m = document.getElementById("modal");
    if (m) m.innerHTML = "";
    if (lastTrigger && typeof lastTrigger.focus === "function") {
      try {
        lastTrigger.focus();
      } catch {
        // element may have been swapped out of the DOM since opening;
        // best-effort restore, swallow failures rather than throw.
      }
    }
    lastTrigger = null;
  };

  // Public: call from each modal's ``x-init`` on the overlay root.
  // Adds the Tab focus-trap and moves focus to the first focusable
  // element (or one with ``autofocus``) inside the modal.
  window.setupModal = function setupModal(overlayEl) {
    if (!overlayEl) return;
    const inner = overlayEl.querySelector(".modal") || overlayEl;
    const autofocus = inner.querySelector("[autofocus]");
    const focusables = focusableWithin(inner);
    if (autofocus) {
      autofocus.focus();
    } else if (focusables.length) {
      focusables[0].focus();
    }
    inner.addEventListener("keydown", (e) => {
      if (e.key !== "Tab") return;
      const items = focusableWithin(inner);
      if (items.length === 0) return;
      const first = items[0];
      const last = items[items.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    });
  };
})();
