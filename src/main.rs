use chrono::{DateTime, Local, LocalResult, NaiveDateTime, NaiveTime, TimeZone, Utc};
use clap::{Parser, Subcommand};
use colored::Colorize;
use std::collections::BTreeMap;
use std::io::{self, Write};
use tm::db::{Database, DbError};
use tm::ipc::{DaemonMessage, notify_daemon};

#[derive(Parser)]
#[command(name = "tm")]
#[command(version, about = "Time tracking CLI for projects and tasks", long_about = None)]
struct Cli {
    /// Disable colored output
    #[arg(long, global = true)]
    no_color: bool,

    #[command(subcommand)]
    command: Commands,
}

fn setup_colors(no_color: bool) {
    if no_color || std::env::var("NO_COLOR").is_ok() {
        colored::control::set_override(false);
    }
}

#[derive(Subcommand)]
enum Commands {
    /// Start tracking time for a project and task
    Start {
        /// Project name
        project: String,
        /// Task name
        task: String,
        /// Round start and end times to nearest 30 minutes when stopping
        #[arg(short, long)]
        round: bool,
        /// Override the start timestamp. Accepts HH:MM, HH:MM:SS, YYYY-MM-DD HH:MM, YYYY-MM-DD HH:MM:SS, or RFC3339
        #[arg(long, conflicts_with = "from_last_stop")]
        started_at: Option<String>,
        /// Start from the stop time of the most recently finished entry
        #[arg(long)]
        from_last_stop: bool,
    },
    /// Amend an existing time entry by ID
    Amend {
        /// Time entry ID from `tm log`
        id: String,
        /// Override the start timestamp. Accepts HH:MM, HH:MM:SS, YYYY-MM-DD HH:MM, YYYY-MM-DD HH:MM:SS, or RFC3339
        #[arg(long)]
        started_at: Option<String>,
        /// Override the stop timestamp. Accepts HH:MM, HH:MM:SS, YYYY-MM-DD HH:MM, YYYY-MM-DD HH:MM:SS, or RFC3339
        #[arg(long)]
        stopped_at: Option<String>,
    },
    /// Stop the currently active time entry
    Stop,
    /// Show the currently active task
    Status,
    /// Show all projects and tasks with logged time
    Log {
        /// Show only billable times (for billing reports)
        #[arg(short, long)]
        billable: bool,
        /// Show entries grouped by day
        #[arg(long)]
        daily: bool,
    },
    /// Clear all entries from the database
    Clear,
    /// Continue tracking the last stopped task
    Continue,
    /// Merge today's fragmented entries per task
    #[command(alias = "compact")]
    Squash {
        /// Day to squash in YYYY-MM-DD format
        #[arg(long, conflicts_with = "yesterday")]
        day: Option<String>,
        /// Squash entries for yesterday
        #[arg(long)]
        yesterday: bool,
    },
    /// Cancel the current entry without saving
    Cancel,
}

fn format_duration(total_seconds: i64) -> String {
    if total_seconds <= 0 {
        return "0s".to_string();
    }

    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    let mut parts = Vec::new();

    if hours > 0 {
        parts.push(format!("{}h", hours));
    }
    if minutes > 0 {
        parts.push(format!("{}m", minutes));
    }
    if seconds > 0 || parts.is_empty() {
        parts.push(format!("{}s", seconds));
    }

    parts.join(" ")
}

fn parse_timestamp(input: &str) -> std::result::Result<DateTime<Utc>, String> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(input) {
        return Ok(dt.with_timezone(&Utc));
    }

    for format in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(naive_dt) = NaiveDateTime::parse_from_str(input, format) {
            return match Local.from_local_datetime(&naive_dt) {
                LocalResult::Single(dt) => Ok(dt.with_timezone(&Utc)),
                LocalResult::Ambiguous(dt, _) => Ok(dt.with_timezone(&Utc)),
                LocalResult::None => Err(format!("Invalid local datetime: {}", input)),
            };
        }
    }

    for format in ["%H:%M:%S", "%H:%M"] {
        if let Ok(time) = NaiveTime::parse_from_str(input, format) {
            let today = Local::now().date_naive();
            let naive_dt = today.and_time(time);
            return match Local.from_local_datetime(&naive_dt) {
                LocalResult::Single(dt) => Ok(dt.with_timezone(&Utc)),
                LocalResult::Ambiguous(dt, _) => Ok(dt.with_timezone(&Utc)),
                LocalResult::None => Err(format!("Invalid local time: {}", input)),
            };
        }
    }

    Err("Unsupported format. Use HH:MM, HH:MM:SS, YYYY-MM-DD HH:MM, YYYY-MM-DD HH:MM:SS, or RFC3339.".to_string())
}

fn cmd_start(
    db: &Database,
    project: &str,
    task: &str,
    round: bool,
    started_at: Option<&str>,
    from_last_stop: bool,
) {
    match db.get_active_entry() {
        Ok(Some(entry)) => {
            return eprintln!(
                "Already tracking: {} / {}. Stop it first.",
                entry.project_name, entry.task_name
            );
        }
        Err(e) => return eprintln!("Error checking active entry: {}", e),
        Ok(None) => {}
    }

    let proj = match db.get_or_create_project(project) {
        Ok(p) => p,
        Err(e) => return eprintln!("Error creating project: {}", e),
    };

    let task_entry = match db.get_or_create_task(proj.id, task) {
        Ok(t) => t,
        Err(e) => return eprintln!("Error creating task: {}", e),
    };

    let started_at = if let Some(value) = started_at {
        match parse_timestamp(value) {
            Ok(dt) => dt,
            Err(e) => return eprintln!("Error parsing --started-at: {}", e),
        }
    } else if from_last_stop {
        match db.get_last_stopped_at() {
            Ok(Some(dt)) => dt,
            Ok(None) => {
                return eprintln!("No previous stopped entry to infer the start time from.");
            }
            Err(e) => return eprintln!("Error getting last stopped entry: {}", e),
        }
    } else {
        Utc::now()
    };

    if let Err(e) = db.start_time_entry(task_entry.id, round, started_at) {
        return eprintln!("Error starting time entry: {}", e);
    }

    if round {
        println!("Started tracking (rounded): {} / {}", project, task);
    } else {
        println!("Started tracking: {} / {}", project, task);
    }
    notify_daemon(&DaemonMessage::Started {
        project: project.to_string(),
        task: task.to_string(),
        started_at,
    });
}

fn cmd_amend(db: &Database, id: &str, started_at: Option<&str>, stopped_at: Option<&str>) {
    if started_at.is_none() && stopped_at.is_none() {
        return eprintln!("Nothing to amend. Provide --started-at and/or --stopped-at.");
    }

    let started_at = match started_at {
        Some(value) => match parse_timestamp(value) {
            Ok(dt) => Some(dt),
            Err(e) => return eprintln!("Error parsing --started-at: {}", e),
        },
        None => None,
    };

    let stopped_at = match stopped_at {
        Some(value) => match parse_timestamp(value) {
            Ok(dt) => Some(dt),
            Err(e) => return eprintln!("Error parsing --stopped-at: {}", e),
        },
        None => None,
    };

    match db.amend_time_entry(id, started_at, stopped_at) {
        Ok(_) => println!("Amended entry {}.", id),
        Err(DbError::EntryNotFound) => eprintln!("Time entry {} not found.", id),
        Err(DbError::InvalidTimeRange) => {
            eprintln!("Invalid time range: --stopped-at must be after --started-at.")
        }
        Err(e) => eprintln!("Error amending time entry: {}", e),
    }
}

fn cmd_stop(db: &Database) {
    match db.stop_active_entry() {
        Ok((_rounded, billable_times)) => {
            if let Some((start, end)) = billable_times {
                let start_str = start.with_timezone(&Local).format("%H:%M");
                let end_str = end.with_timezone(&Local).format("%H:%M");
                println!("Stopped tracking (rounded: {} to {}).", start_str, end_str);
            } else {
                println!("Stopped tracking.");
            }
            notify_daemon(&DaemonMessage::Stopped);
        }
        Err(DbError::NoActiveEntry) => println!("No active time entry."),
        Err(e) => eprintln!("Error stopping time entry: {}", e),
    }
}

fn cmd_status(db: &Database) {
    match db.get_active_entry() {
        Ok(Some(entry)) => {
            let elapsed = Utc::now()
                .signed_duration_since(entry.started_at)
                .num_seconds();
            println!(
                "Tracking: {} / {} ({})",
                entry.project_name,
                entry.task_name,
                format_duration(elapsed)
            );
        }
        Ok(None) => println!("No active time entry."),
        Err(e) => eprintln!("Error getting status: {}", e),
    }
}

fn cmd_cancel(db: &Database) {
    match db.cancel_active_entry() {
        Ok(_) => {
            println!("Cancelled active entry.");
            notify_daemon(&DaemonMessage::Cancelled);
        }
        Err(DbError::NoActiveEntry) => println!("No active time entry."),
        Err(e) => eprintln!("Error cancelling entry: {}", e),
    }
}

fn cmd_continue(db: &Database) {
    match db.get_active_entry() {
        Ok(Some(entry)) => {
            return eprintln!(
                "Already tracking: {} / {}. Stop it first.",
                entry.project_name, entry.task_name
            );
        }
        Err(e) => return eprintln!("Error checking active entry: {}", e),
        Ok(None) => {}
    }

    let (_, task_id, project_name, task_name, round_on_stop) = match db.get_last_stopped_task() {
        Ok(Some(entry)) => entry,
        Ok(None) => return eprintln!("No previous task to continue."),
        Err(e) => return eprintln!("Error getting last task: {}", e),
    };

    let started_at = Utc::now();

    if let Err(e) = db.start_time_entry(task_id, round_on_stop, started_at) {
        return eprintln!("Error starting time entry: {}", e);
    }

    println!("Continuing: {} / {}", project_name, task_name);
    notify_daemon(&DaemonMessage::Continued {
        project: project_name,
        task: task_name,
        started_at,
    });
}

fn parse_day(input: &str) -> std::result::Result<chrono::NaiveDate, String> {
    chrono::NaiveDate::parse_from_str(input, "%Y-%m-%d")
        .map_err(|_| "Unsupported date format. Use YYYY-MM-DD.".to_string())
}

fn cmd_squash(db: &Database, day: Option<&str>, yesterday: bool) {
    let day = if yesterday {
        Local::now().date_naive() - chrono::Duration::days(1)
    } else if let Some(day) = day {
        match parse_day(day) {
            Ok(day) => day,
            Err(e) => return eprintln!("Error parsing --day: {}", e),
        }
    } else {
        Local::now().date_naive()
    };

    match db.squash_day(day) {
        Ok(result) => {
            if result.squashed_tasks == 0 {
                println!(
                    "No fragmented entries found for {}.",
                    day.format("%Y-%m-%d")
                );
            } else {
                println!(
                    "Squashed {} task(s) for {}, removed {} entries.",
                    result.squashed_tasks,
                    day.format("%Y-%m-%d"),
                    result.deleted_entries
                );
            }
        }
        Err(e) => eprintln!("Error squashing entries: {}", e),
    }
}

fn cmd_clear(db: &Database) {
    print!("Are you sure you want to clear all entries? [y/N] ");
    io::stdout().flush().unwrap();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        eprintln!("Error reading input.");
        return;
    }

    let input = input.trim().to_lowercase();
    if input != "y" && input != "yes" {
        println!("Aborted.");
        return;
    }

    match db.clear_all() {
        Ok(_) => println!("All entries cleared."),
        Err(e) => eprintln!("Error clearing entries: {}", e),
    }
}

struct EntryDisplayInfo {
    day: chrono::NaiveDate,
    time_range: String,
    start: DateTime<Local>,
    end: DateTime<Local>,
    duration_seconds: i64,
    is_active: bool,
}

fn entry_display_info(
    entry: &tm::db::TimeEntryDetail,
    billable_only: bool,
    now_local: DateTime<Local>,
) -> EntryDisplayInfo {
    let (start, end, duration_seconds, is_active) = if billable_only {
        let start = entry
            .billable_started_at
            .unwrap_or(entry.started_at)
            .with_timezone(&Local);
        let stopped_at = entry.billable_stopped_at.or(entry.stopped_at);
        let end = stopped_at
            .map(|dt| dt.with_timezone(&Local))
            .unwrap_or(now_local);
        let duration_seconds = entry.billable_seconds.unwrap_or(entry.duration_seconds);
        (start, end, duration_seconds, stopped_at.is_none())
    } else {
        let start = entry.started_at.with_timezone(&Local);
        let end = entry
            .stopped_at
            .map(|dt| dt.with_timezone(&Local))
            .unwrap_or(now_local);
        (
            start,
            end,
            entry.duration_seconds,
            entry.stopped_at.is_none(),
        )
    };

    EntryDisplayInfo {
        day: start.date_naive(),
        time_range: if is_active {
            format!("{} to ...", start.format("%H:%M"))
        } else {
            format!("{} to {}", start.format("%H:%M"), end.format("%H:%M"))
        },
        start,
        end,
        duration_seconds,
        is_active,
    }
}

fn cmd_log_daily(projects: &[tm::db::ProjectSummary], billable_only: bool) {
    struct DailyRange {
        start: DateTime<Local>,
        end: DateTime<Local>,
        label: String,
    }

    struct DailyTask {
        project_name: String,
        task_name: String,
        total_seconds: i64,
        ranges: Vec<DailyRange>,
        has_active_range: bool,
    }

    let now_local = Local::now();
    let mut days: BTreeMap<chrono::NaiveDate, Vec<DailyTask>> = BTreeMap::new();

    for project in projects {
        for task in &project.tasks {
            for entry in &task.entries {
                let info = entry_display_info(entry, billable_only, now_local);
                let day_tasks = days.entry(info.day).or_default();

                if let Some(existing) = day_tasks.iter_mut().find(|daily_task| {
                    daily_task.project_name == project.project_name
                        && daily_task.task_name == task.task_name
                }) {
                    existing.total_seconds += info.duration_seconds;
                    existing.has_active_range |= info.is_active;
                    existing.ranges.push(DailyRange {
                        start: info.start,
                        end: info.end,
                        label: info.time_range,
                    });
                } else {
                    day_tasks.push(DailyTask {
                        project_name: project.project_name.clone(),
                        task_name: task.task_name.clone(),
                        total_seconds: info.duration_seconds,
                        has_active_range: info.is_active,
                        ranges: vec![DailyRange {
                            start: info.start,
                            end: info.end,
                            label: info.time_range,
                        }],
                    });
                }
            }
        }
    }

    if days.is_empty() {
        println!("No projects found.");
        return;
    }

    for (day, mut tasks) in days.into_iter().rev() {
        let mut day_start: Option<DateTime<Local>> = None;
        let mut day_end: Option<DateTime<Local>> = None;
        let mut day_total_seconds = 0;

        for task in &mut tasks {
            task.ranges.sort_by_key(|range| range.start);
            day_total_seconds += task.total_seconds;

            if let Some(range) = task.ranges.first() {
                day_start = Some(match day_start {
                    Some(current) => current.min(range.start),
                    None => range.start,
                });
            }

            if let Some(range) = task.ranges.last() {
                day_end = Some(match day_end {
                    Some(current) => current.max(range.end),
                    None => range.end,
                });
            }
        }

        tasks.sort_by(|a, b| {
            a.ranges[0]
                .start
                .cmp(&b.ranges[0].start)
                .then_with(|| a.project_name.cmp(&b.project_name))
                .then_with(|| a.task_name.cmp(&b.task_name))
        });

        let day_start = day_start.unwrap_or(now_local);
        let day_end = day_end.unwrap_or(now_local);

        println!(
            "{} {} (from {}{}, total time: {})",
            "day:".blue().bold(),
            day.format("%d %B %Y").to_string().cyan().bold(),
            day_start.format("%H:%M").to_string().dimmed(),
            if tasks.iter().any(|task| task.has_active_range) {
                " and counting".dimmed().to_string()
            } else {
                format!(" to {}", day_end.format("%H:%M"))
                    .dimmed()
                    .to_string()
            },
            format_duration(day_total_seconds).yellow()
        );

        for task in tasks {
            let ranges = task
                .ranges
                .into_iter()
                .map(|range| range.label)
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "  {} {} / {} ({}) {}",
                "-".dimmed(),
                task.project_name.cyan(),
                task.task_name.green(),
                format_duration(task.total_seconds).yellow(),
                ranges.dimmed()
            );
        }
    }
}

fn cmd_log(db: &Database, billable_only: bool, daily: bool) {
    match db.get_log() {
        Ok(projects) => {
            if projects.is_empty() {
                println!("No projects found.");
                return;
            }

            if daily {
                cmd_log_daily(&projects, billable_only);
                return;
            }

            let now_local = Local::now();

            for project in projects {
                let project_time_str = if billable_only {
                    format_duration(project.billable_seconds)
                } else if project.billable_seconds != project.total_seconds {
                    format!(
                        "{} / billable: {}",
                        format_duration(project.total_seconds),
                        format_duration(project.billable_seconds)
                    )
                } else {
                    format_duration(project.total_seconds)
                };

                println!(
                    "{} {} (total time: {})",
                    "project:".blue().bold(),
                    project.project_name.cyan().bold(),
                    project_time_str.yellow()
                );

                for task in &project.tasks {
                    let task_time_str = if billable_only {
                        format_duration(task.billable_seconds)
                    } else if task.billable_seconds != task.total_seconds {
                        format!(
                            "{} / billable: {}",
                            format_duration(task.total_seconds),
                            format_duration(task.billable_seconds)
                        )
                    } else {
                        format_duration(task.total_seconds)
                    };

                    println!(
                        "  {} {} (total time: {})",
                        "-".dimmed(),
                        format!("task: {}", task.task_name).green(),
                        task_time_str.yellow()
                    );

                    // Collect entries for column width calculation
                    let mut entries_data: Vec<(String, String, String, String)> = Vec::new();

                    for entry in &task.entries {
                        let info = entry_display_info(entry, billable_only, now_local);
                        let date_str = info.day.format("%d %B %Y").to_string();
                        let duration = if billable_only {
                            format_duration(
                                entry.billable_seconds.unwrap_or(entry.duration_seconds),
                            )
                        } else {
                            match entry.billable_seconds {
                                Some(billable) if billable != entry.duration_seconds => {
                                    format!(
                                        "{} / billable: {}",
                                        format_duration(entry.duration_seconds),
                                        format_duration(billable)
                                    )
                                }
                                _ => format_duration(entry.duration_seconds),
                            }
                        };

                        entries_data.push((date_str, info.time_range, duration, entry.id.clone()));
                    }

                    // Calculate max widths
                    let max_date_len = entries_data.iter().map(|e| e.0.len()).max().unwrap_or(0);
                    let max_time_len = entries_data.iter().map(|e| e.1.len()).max().unwrap_or(0);
                    let max_duration_len =
                        entries_data.iter().map(|e| e.2.len()).max().unwrap_or(0);

                    for (date_str, time_range, duration, id) in &entries_data {
                        println!(
                            "      {} {:date_width$}   {:time_width$}   ({:>duration_width$})   {}",
                            "-".dimmed(),
                            date_str.white(),
                            time_range.dimmed(),
                            duration.yellow(),
                            id.bright_black(),
                            date_width = max_date_len,
                            time_width = max_time_len,
                            duration_width = max_duration_len,
                        );
                    }
                }
            }
        }
        Err(e) => eprintln!("Error getting log: {}", e),
    }
}

fn main() {
    let cli = Cli::parse();

    setup_colors(cli.no_color);

    let db = match Database::open() {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Error opening database: {}", e);
            std::process::exit(1);
        }
    };

    match cli.command {
        Commands::Start {
            project,
            task,
            round,
            started_at,
            from_last_stop,
        } => cmd_start(
            &db,
            &project,
            &task,
            round,
            started_at.as_deref(),
            from_last_stop,
        ),
        Commands::Amend {
            id,
            started_at,
            stopped_at,
        } => cmd_amend(&db, &id, started_at.as_deref(), stopped_at.as_deref()),
        Commands::Stop => cmd_stop(&db),
        Commands::Status => cmd_status(&db),
        Commands::Log { billable, daily } => cmd_log(&db, billable, daily),
        Commands::Clear => cmd_clear(&db),
        Commands::Continue => cmd_continue(&db),
        Commands::Squash { day, yesterday } => cmd_squash(&db, day.as_deref(), yesterday),
        Commands::Cancel => cmd_cancel(&db),
    }
}
