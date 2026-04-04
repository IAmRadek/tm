use chrono::{Local, Utc};
use clap::{Parser, Subcommand};
use colored::Colorize;
use std::io::{self, Write};
use tm::db::{Database, DbError};
use tm::ipc::{notify_daemon, DaemonMessage};

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
    },
    /// Clear all entries from the database
    Clear,
    /// Continue tracking the last stopped task
    Continue,
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

fn cmd_start(db: &Database, project: &str, task: &str, round: bool) {
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

    if let Err(e) = db.start_time_entry(task_entry.id, round) {
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
        started_at: Utc::now(),
    });
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

    let (_, task_id, project_name, task_name) = match db.get_last_stopped_task() {
        Ok(Some(entry)) => entry,
        Ok(None) => return eprintln!("No previous task to continue."),
        Err(e) => return eprintln!("Error getting last task: {}", e),
    };

    if let Err(e) = db.start_time_entry(task_id, false) {
        return eprintln!("Error starting time entry: {}", e);
    }

    println!("Continuing: {} / {}", project_name, task_name);
    notify_daemon(&DaemonMessage::Continued {
        project: project_name,
        task: task_name,
        started_at: Utc::now(),
    });
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

fn cmd_log(db: &Database, billable_only: bool) {
    match db.get_log() {
        Ok(projects) => {
            if projects.is_empty() {
                println!("No projects found.");
                return;
            }

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
                        let date_str = entry.date.format("%d %B %Y").to_string();

                        let (start_time, end_time) = if billable_only {
                            // Use billable times if available, otherwise fall back to real times
                            let start = entry
                                .billable_started_at
                                .unwrap_or(entry.started_at)
                                .with_timezone(&Local);
                            let end = match entry.billable_stopped_at {
                                Some(t) => t.with_timezone(&Local).format("%H:%M").to_string(),
                                None => match entry.stopped_at {
                                    Some(t) => t.with_timezone(&Local).format("%H:%M").to_string(),
                                    None => "now".to_string(),
                                },
                            };
                            (start.format("%H:%M").to_string(), end)
                        } else {
                            let start_local = entry.started_at.with_timezone(&Local);
                            let end_time = match entry.stopped_at {
                                Some(stopped) => {
                                    stopped.with_timezone(&Local).format("%H:%M").to_string()
                                }
                                None => "now".to_string(),
                            };
                            (start_local.format("%H:%M").to_string(), end_time)
                        };

                        let time_range = format!("{} to {}", start_time, end_time);
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

                        entries_data.push((date_str, time_range, duration, entry.id.clone()));
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
        } => cmd_start(&db, &project, &task, round),
        Commands::Stop => cmd_stop(&db),
        Commands::Status => cmd_status(&db),
        Commands::Log { billable } => cmd_log(&db, billable),
        Commands::Clear => cmd_clear(&db),
        Commands::Continue => cmd_continue(&db),
        Commands::Cancel => cmd_cancel(&db),
    }
}
