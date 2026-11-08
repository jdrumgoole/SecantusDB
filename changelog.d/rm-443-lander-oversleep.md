### A batch lander that overslept is filed as a watch item

`scripts/land_batch.sh` polls the integration branch every five minutes. On 2026-11-08 one of its `sleep 300` calls was still sleeping 63 minutes later, so a finished fix waited an hour for its snapshot PR, until the sleep was killed by hand. The batch's other sessions on the same machine kept running, so the machine was not asleep. It is filed as RM-443, with what to capture on a recurrence.

#### Changed

- Filed RM-443, a tooling watch item.
