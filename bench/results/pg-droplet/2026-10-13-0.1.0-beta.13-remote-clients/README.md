# 0.1.0-beta.13, measured again on the droplets that measured 0.1.0-beta.14

The same three droplets as `../2026-10-13-0.1.0-beta.14-remote-clients/`,
an hour later, so the two releases can be compared on one machine. Neither
server used TLS. Five trials a row, except the update sweep.

`update-distinct.json` is a repeat of the update sweep alone, seven trials.
`update-distinct.first-run.json` is what the full run recorded for it: with
eight clients, `secantusd-pg` gave 12,418 and 12,209 statements a second
and then 8,019, 8,156 and 9,000. The repeat gave 12,212 to 12,811 in all
seven, and the run of 2026-10-12 gave 12,476 to 12,959. The cause of the
three slow trials is not known; `tasks/backlog.md` has the entry.
`facts.txt` is the repeat's.
