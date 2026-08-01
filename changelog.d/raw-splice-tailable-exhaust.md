### Change-stream and exhaust replies stop re-encoding every document

The last two survivors of the reply-path materialization (Finding 2) are
gone. A change stream's tailable getMore decoded every event blob into a
document and re-encoded it onto the wire — even though the only thing the
handler needed from the batch was the last event's `_id` for the
postBatchResumeToken. And the exhaust streamer round-tripped every batch
through an owned document array (plus a full clone of each batch) between
pulling it from the cursor registry and framing it. Both now splice the
pre-encoded blobs straight onto the wire like the ordinary find/getMore
path has since the RecordId era. Measured: change-stream drain +22%
(105k → 128k events/s), exhaust-cursor drain +26% (1.20M → 1.52M docs/s).

#### Changed
- `secantus-commands`: the tailable getMore hands its event blobs to the
  wire encoder undecoded; the postBatchResumeToken decodes only the final
  blob (as it always did).
- `secantus-server`: the exhaust streamer threads the pre-encoded batch
  through every `moreToCome` frame (`encode_cursor_reply` splice) instead
  of materialising and cloning it per frame; `materialize_batch` is gone.
