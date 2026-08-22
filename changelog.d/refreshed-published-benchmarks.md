### Republished the performance numbers, measured properly

Every figure on the benchmark and concurrency pages has been re-measured. The
per-operation latency table now comes from a dedicated cloud instance against
**mongod 8.0.29**; the writer-scaling sweep from a quiet 12-core workstation,
verified by two independent runs agreeing to within 1.2%.

#### Changed

- **Latency ratios look worse, and SecantusDB is not the reason.** The
  reference moved from mongod 6.0.16 to 8.0.29. Every "×mongod" figure is a
  ratio, so a faster denominator lowers the score: insert reads 2.0× where it
  read 1.4×, while SecantusDB's own absolute timing barely moved. Publishing
  against a three-major-version-old mongod flattered us. The results file now
  records the mongod version so this cannot silently recur.
- **Concurrency scaling looks better, and that is a fixed measurement rather
  than a faster engine.** The sweep used to share one store across every writer
  count, so later rows carried everything the earlier rows wrote. Removing that
  lifted every engine — including mongod, unchanged code, by 14% at eight
  writers. The Rust server now measures 3.0× scaling at eight writers.
