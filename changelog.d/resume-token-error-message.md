### The resume-token error now carries mongod's wording

A change stream whose pipeline modifies the event `_id` is rejected with error
280 on both servers — but the Python server's message was its own paraphrase,
ending "makes it unusable for resuming".

#### Fixed

- The Python server now returns mongod's exact text, including the sentence
  drivers assert on: *"Only transformations that retain the unmodified `_id`
  field are allowed."* The Rust server already carried it. `libmongoc` checks
  that string, so the paraphrase failed
  `/change_stream/live/missing_resume_token` and `/invalid_resume_token` in the
  C gauge — 758 passed / 10 failed → **760 passed / 8 failed** (98.7% → 99.0%).
  pymongo does not assert on the message, so only a stricter driver could
  surface it.
