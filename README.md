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

# Start at a specific time today
tm start myproject "implement feature" --at 09:30

# Start at a specific date and time
tm start myproject "implement feature" --at "2026-04-07 09:30"

# Start from the stop time of the last finished entry
tm start myproject "review PR" --from-last-stop

# Amend an existing entry by ID
tm amend <id> --started-at "2026-04-07 09:30"
tm amend <id> --stopped-at "2026-04-07 11:00"
tm amend <id> --started-at "2026-04-07 09:30" --stopped-at "2026-04-07 11:00"

# Check status
tm status

# One line for a status bar: "project / task · elapsed", exit code 1 when idle
tm status --short

# Stop tracking
tm stop

# Stop at a specific time
tm stop --at 12:00

# Continue last task
tm continue

# Merge today's fragmented entries per task
tm squash

# Merge yesterday's fragmented entries
tm squash --yesterday

# Merge fragmented entries for a specific day
tm squash --day 2026-04-07

# View today's log
tm log

# View today's log with time ranges
tm log --verbose

# View all tasks and entries
tm log --tasks

# View log grouped by day
tm log --daily

# View logged days in a month calendar
tm log --calendar
tm log --calendar-full
tm log --calendar-full --all

# Cancel current entry
tm cancel

# Clear all data
tm clear
```

## How it works

- Data stored in `~/.config/tm/data.sqlite`
- Time entries grouped by project and task
- `tm squash` merges today's stopped fragments per task into one wider entry
- `--at` on `tm start` and `tm stop` sets the time you track

## License

MIT
