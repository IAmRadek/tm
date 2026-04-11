# tm

A minimal CLI time tracker for projects and tasks.

## Install

```bash
cargo install --path .
```

## Usage

```bash
# Start tracking
tm start <project> <task>
tm start myproject "implement feature"

# Start with 30-min rounding (for billing)
tm start myproject "implement feature" --round

# Start at a specific time today
tm start myproject "implement feature" --started-at 09:30

# Start at a specific date and time
tm start myproject "implement feature" --started-at "2026-04-07 09:30"

# Amend an existing entry by ID
tm amend <id> --started-at "2026-04-07 09:30"
tm amend <id> --stopped-at "2026-04-07 11:00"
tm amend <id> --started-at "2026-04-07 09:30" --stopped-at "2026-04-07 11:00"

# Check status
tm status

# Stop tracking
tm stop

# Continue last task
tm continue

# Merge today's fragmented entries per task
tm squash

# View log
tm log

# View log grouped by day
tm log --daily

# View billable times only
tm log --billable

# Cancel current entry
tm cancel

# Clear all data
tm clear
```

## How it works

- Data stored in `~/.config/tm/data.sqlite`
- Time entries grouped by project and task
- `tm squash` merges today's stopped fragments per task into one wider entry
- Optional rounding to nearest 30 minutes for billing
- Tracks both actual and billable time

## License

MIT
