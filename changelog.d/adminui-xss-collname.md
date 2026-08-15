### Admin UI: collection names can no longer inject script

The collections page built its per-row modify/rename toggle keys by
splicing the collection name straight into an Alpine.js directive
(`@click`, `x-show`). Jinja HTML-escaped the name, but the browser
decodes those entities back before Alpine compiles the attribute as a
JavaScript expression via `new Function`, so a collection name
containing a single quote — no character restriction exists on
collection names — broke out of the toggle-key literal and ran
arbitrary script in the admin operator's authenticated session on page
render. Because the Mongo wire port is unauthenticated by default, this
was a pivot from an anonymous wire client to the admin UI's session.

The toggle keys are now the loop's row index (an integer), so no
attacker-controlled string ever reaches the Alpine expression context.
The name still renders, safely escaped, in the row's link, form
actions, and rename field.

#### Security

- Stored XSS on the admin collections page: a collection name with a
  `'` executed script in the operator's session on render (#835). Row
  toggle keys are now integer indices, keeping the name out of every
  Alpine.js directive.
