### Docs: the benchmark pages get charts

`docs/benchmark.md` and `docs/concurrency.md` now open their result
sections with inline SVG charts — grouped latency-multiplier bars against
a mongod = 1x reference, and the three-server concurrency-scaling lines —
theme-aware for furo's light and dark modes (palette validated for both
surfaces), with native tooltips and the tables kept as the data view.
Matches the charts on secantusdb.com/performance.html.
