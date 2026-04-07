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

# Check status
tm status

# Stop tracking
tm stop

# Continue last task
tm continue

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
- Optional rounding to nearest 30 minutes for billing
- Tracks both actual and billable time

## License

MIT
