# tidewater — nightly backups for the lab

## Decision

- We adopt tidewater on 1 November and retire the hand-written rsync scripts that week.
- Personal laptops stay out of scope until the storage box is upgraded; revisit in March.
- Not decided yet: whether to buy a second storage box. The budget meeting in February settles it.

## How a run works

- A run starts at 02:00 and visits one machine at a time, so the network stays quiet.
- Machines that are asleep are skipped and picked up again on the next run.
- After the first pass, only changed blocks are sent.

## Retention

- We keep 14 daily, 8 weekly and 12 monthly snapshots; anything older is pruned on Sundays.

## Getting a file back

- Anyone can pull a file out of their own machine's snapshots: `tidewater restore ~/notes/ch3.md --from 3d`.
- Whole-machine restores go through the lab manager.

## Off-site copy

- Each Sunday the weekly snapshot is encrypted and shipped to the storage box in the chemistry wing.
- Losing the entire lab would cost us at most one week of work.

## When a run fails

- tidewater posts to the #lab-ops channel.
- If two runs in a row fail, it pages the person on this week's rota.
