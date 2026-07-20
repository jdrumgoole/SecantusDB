### Admin change-stream tail stops polling when the client disconnects

The admin UI's change-stream WebSocket tail only noticed a disconnected
client the next time it tried to *send* an event. On a quiet stream — one
watching an idle collection — that meant closing the browser tab left the
server looping `try_next` against the change stream indefinitely, holding a
tailable cursor open until the app shut down. It also left an orphaned poll
thread behind: `asyncio.to_thread` can't be cancelled mid-flight, and the
per-poll `try_next` had no bounded wait, so under heavy CI load such an
orphan could linger long enough to wedge a test worker (a recurring
intermittent CI crash in `test_ws_changes_streams_collection_event`).

The tail now races each `try_next` poll against a disconnect watcher, so a
quiet stream notices a gone client immediately and closes its cursor, and
bounds each poll's server-side awaitData wait (`max_await_time_ms=500`) so
no orphaned poll can linger. Event delivery to a live client is unchanged.

#### Fixed

- Admin UI: the change-stream tail stops polling and releases its cursor as
  soon as the client disconnects, instead of only on the next event send.
- Bounded the per-poll awaitData wait so an orphaned poll thread left behind
  on disconnect frees promptly, hardening against an intermittent CI worker
  crash under load.
