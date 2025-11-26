use chrono::{DateTime, Timelike, Utc};
use rand::Rng;
use rusqlite::{Connection, Result as SqliteResult, params};
use std::env;
use std::fs;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DbError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Could not determine config directory")]
    NoConfigDir,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("No active time entry")]
    NoActiveEntry,
}

pub type Result<T> = std::result::Result<T, DbError>;

pub type BillableTimes = Option<(DateTime<Utc>, DateTime<Utc>)>;

#[derive(Debug, Clone)]
pub struct Project {
    pub id: i64,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct Task {
    pub id: i64,
    pub project_id: i64,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct TimeEntry {
    pub id: String,
    pub task_id: i64,
    pub started_at: DateTime<Utc>,
    pub stopped_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct ActiveEntry {
    pub project_name: String,
    pub task_name: String,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct TimeEntryDetail {
    pub id: String,
    pub date: chrono::NaiveDate,
    pub started_at: DateTime<Utc>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub duration_seconds: i64,
    pub billable_seconds: Option<i64>,
    pub billable_started_at: Option<DateTime<Utc>>,
    pub billable_stopped_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct TaskSummary {
    pub task_name: String,
    pub total_seconds: i64,
    pub billable_seconds: i64,
    pub entries: Vec<TimeEntryDetail>,
}

#[derive(Debug, Clone)]
pub struct ProjectSummary {
    pub project_name: String,
    pub total_seconds: i64,
    pub billable_seconds: i64,
    pub tasks: Vec<TaskSummary>,
}

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open() -> Result<Self> {
        let db_path = Self::get_db_path()?;

        // Ensure parent directory exists
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(&db_path)?;
        let db = Database { conn };
        db.init_schema()?;
        Ok(db)
    }

    fn get_db_path() -> Result<PathBuf> {
        let home = env::var("HOME").map_err(|_| DbError::NoConfigDir)?;
        Ok(PathBuf::from(home).join(".config/tm/data.sqlite"))
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS projects (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS tasks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project_id INTEGER NOT NULL,
                name TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (project_id) REFERENCES projects(id),
                UNIQUE(project_id, name)
            );

            CREATE TABLE IF NOT EXISTS time_entries (
                id TEXT PRIMARY KEY,
                task_id INTEGER NOT NULL,
                started_at TEXT NOT NULL DEFAULT (datetime('now')),
                stopped_at TEXT,
                billable_started_at TEXT,
                billable_stopped_at TEXT,
                round_on_stop INTEGER NOT NULL DEFAULT 0,
                FOREIGN KEY (task_id) REFERENCES tasks(id)
            );

            CREATE INDEX IF NOT EXISTS idx_tasks_project_id ON tasks(project_id);
            CREATE INDEX IF NOT EXISTS idx_time_entries_task_id ON time_entries(task_id);
            CREATE INDEX IF NOT EXISTS idx_time_entries_stopped_at ON time_entries(stopped_at);
            ",
        )?;
        Ok(())
    }

    /// Get or create a project by name
    pub fn get_or_create_project(&self, name: &str) -> Result<Project> {
        // Try to find existing project
        let maybe_project: SqliteResult<Project> = self.conn.query_row(
            "SELECT id, name, created_at FROM projects WHERE name = ?1",
            params![name],
            |row| {
                Ok(Project {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    created_at: row
                        .get::<_, String>(2)?
                        .parse()
                        .unwrap_or_else(|_| Utc::now()),
                })
            },
        );

        match maybe_project {
            Ok(project) => Ok(project),
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                // Create new project
                self.conn
                    .execute("INSERT INTO projects (name) VALUES (?1)", params![name])?;
                let id = self.conn.last_insert_rowid();
                Ok(Project {
                    id,
                    name: name.to_string(),
                    created_at: Utc::now(),
                })
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Get or create a task by name within a project
    pub fn get_or_create_task(&self, project_id: i64, name: &str) -> Result<Task> {
        let maybe_task: SqliteResult<Task> = self.conn.query_row(
            "SELECT id, project_id, name, created_at FROM tasks WHERE project_id = ?1 AND name = ?2",
            params![project_id, name],
            |row| {
                Ok(Task {
                    id: row.get(0)?,
                    project_id: row.get(1)?,
                    name: row.get(2)?,
                    created_at: row.get::<_, String>(3)?.parse().unwrap_or_else(|_| Utc::now()),
                })
            },
        );

        match maybe_task {
            Ok(task) => Ok(task),
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                self.conn.execute(
                    "INSERT INTO tasks (project_id, name) VALUES (?1, ?2)",
                    params![project_id, name],
                )?;
                let id = self.conn.last_insert_rowid();
                Ok(Task {
                    id,
                    project_id,
                    name: name.to_string(),
                    created_at: Utc::now(),
                })
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Generate a random hash ID (similar to git short hash)
    fn generate_hash_id() -> String {
        let mut rng = rand::rng();
        let bytes: [u8; 16] = rng.random();
        bytes
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>()[..12]
            .to_string()
    }

    /// Start a new time entry
    pub fn start_time_entry(&self, task_id: i64, round_on_stop: bool) -> Result<TimeEntry> {
        let id = Self::generate_hash_id();

        self.conn.execute(
            "INSERT INTO time_entries (id, task_id, round_on_stop) VALUES (?1, ?2, ?3)",
            params![id, task_id, round_on_stop],
        )?;

        Ok(TimeEntry {
            id,
            task_id,
            started_at: Utc::now(),
            stopped_at: None,
        })
    }

    /// Round a datetime DOWN to nearest 30 minutes (for start time)
    fn round_down_to_30min(dt: DateTime<Utc>) -> DateTime<Utc> {
        let minutes = dt.minute();

        // Round down: 0-29 → 0, 30-59 → 30
        let rounded_mins = if minutes < 30 { 0 } else { 30 };

        dt.with_minute(rounded_mins)
            .and_then(|t| t.with_second(0))
            .unwrap_or(dt)
    }

    /// Round a datetime UP to nearest 30 minutes (for end time)
    fn round_up_to_30min(dt: DateTime<Utc>) -> DateTime<Utc> {
        let minutes = dt.minute();
        let seconds = dt.second();

        // If exactly on 0 or 30, no rounding needed
        if (minutes == 0 || minutes == 30) && seconds == 0 {
            return dt;
        }

        // Round up: 1-30 → 30, 31-59 → 60 (next hour)
        let rounded_mins = if minutes < 30 { 30 } else { 60 };

        if rounded_mins == 60 {
            dt.with_minute(0)
                .and_then(|t| t.with_second(0))
                .map(|t| t + chrono::Duration::hours(1))
                .unwrap_or(dt)
        } else {
            dt.with_minute(rounded_mins)
                .and_then(|t| t.with_second(0))
                .unwrap_or(dt)
        }
    }

    /// Stop the currently active time entry, returns (rounded, Option<(billable_start, billable_end)>)
    pub fn stop_active_entry(&self) -> Result<(bool, BillableTimes)> {
        // Check if there's an active entry and if it should be rounded
        let (entry_id, started_at_str, round_on_stop): (String, String, bool) = self
            .conn
            .query_row(
                "SELECT id, started_at, round_on_stop FROM time_entries WHERE stopped_at IS NULL LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, i64>(2)? != 0)),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => DbError::NoActiveEntry,
                _ => DbError::Sqlite(e),
            })?;

        let stopped_at = Utc::now();

        if round_on_stop {
            let started_at: DateTime<Utc> = started_at_str
                .parse()
                .or_else(|_| {
                    chrono::NaiveDateTime::parse_from_str(&started_at_str, "%Y-%m-%d %H:%M:%S")
                        .map(|dt| dt.and_utc())
                })
                .unwrap_or_else(|_| Utc::now());

            let billable_start = Self::round_down_to_30min(started_at);
            let billable_stop = Self::round_up_to_30min(stopped_at);

            self.conn.execute(
                "UPDATE time_entries SET stopped_at = ?1, billable_started_at = ?2, billable_stopped_at = ?3 WHERE id = ?4",
                params![
                    stopped_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                    billable_start.format("%Y-%m-%d %H:%M:%S").to_string(),
                    billable_stop.format("%Y-%m-%d %H:%M:%S").to_string(),
                    entry_id
                ],
            )?;

            Ok((true, Some((billable_start, billable_stop))))
        } else {
            self.conn.execute(
                "UPDATE time_entries SET stopped_at = ?1 WHERE id = ?2",
                params![stopped_at.format("%Y-%m-%d %H:%M:%S").to_string(), entry_id],
            )?;

            Ok((false, None))
        }
    }

    /// Cancel the currently active time entry (delete without saving)
    pub fn cancel_active_entry(&self) -> Result<()> {
        let rows_affected = self
            .conn
            .execute("DELETE FROM time_entries WHERE stopped_at IS NULL", [])?;

        if rows_affected == 0 {
            return Err(DbError::NoActiveEntry);
        }

        Ok(())
    }

    /// Get the currently active time entry with project and task names
    pub fn get_active_entry(&self) -> Result<Option<ActiveEntry>> {
        let result: SqliteResult<ActiveEntry> = self.conn.query_row(
            "
            SELECT p.name, t.name, te.started_at
            FROM time_entries te
            JOIN tasks t ON te.task_id = t.id
            JOIN projects p ON t.project_id = p.id
            WHERE te.stopped_at IS NULL
            LIMIT 1
            ",
            [],
            |row| {
                let started_at_str: String = row.get(2)?;
                let started_at = started_at_str
                    .parse()
                    .or_else(|_| {
                        chrono::NaiveDateTime::parse_from_str(&started_at_str, "%Y-%m-%d %H:%M:%S")
                            .map(|dt| dt.and_utc())
                    })
                    .unwrap_or_else(|_| Utc::now());

                Ok(ActiveEntry {
                    project_name: row.get(0)?,
                    task_name: row.get(1)?,
                    started_at,
                })
            },
        );

        match result {
            Ok(entry) => Ok(Some(entry)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get all projects with their tasks and total time, grouped by task
    pub fn get_log(&self) -> Result<Vec<ProjectSummary>> {
        use std::collections::HashMap;

        let mut projects: Vec<ProjectSummary> = Vec::new();

        // Get all projects
        let mut project_stmt = self
            .conn
            .prepare("SELECT id, name FROM projects ORDER BY name")?;

        let project_rows = project_stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;

        for project_result in project_rows {
            let (project_id, project_name) = project_result?;

            // Get all time entries for this project with task names
            let mut entry_stmt = self.conn.prepare(
                "
                SELECT te.id, t.name, te.started_at, te.stopped_at,
                    CAST((julianday(COALESCE(te.stopped_at, datetime('now'))) - julianday(te.started_at)) * 86400 AS INTEGER) as duration,
                    te.billable_started_at, te.billable_stopped_at
                FROM time_entries te
                JOIN tasks t ON te.task_id = t.id
                WHERE t.project_id = ?1
                ORDER BY te.started_at DESC
                "
            )?;

            let entry_rows = entry_stmt.query_map(params![project_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })?;

            // Group entries by task name
            let mut tasks_map: HashMap<String, Vec<TimeEntryDetail>> = HashMap::new();

            for entry_result in entry_rows {
                let (
                    id,
                    task_name,
                    started_at_str,
                    stopped_at_str,
                    duration,
                    billable_start_str,
                    billable_stop_str,
                ): (
                    String,
                    String,
                    String,
                    Option<String>,
                    i64,
                    Option<String>,
                    Option<String>,
                ) = entry_result?;

                let started_at: DateTime<Utc> = started_at_str
                    .parse()
                    .or_else(|_| {
                        chrono::NaiveDateTime::parse_from_str(&started_at_str, "%Y-%m-%d %H:%M:%S")
                            .map(|dt| dt.and_utc())
                    })
                    .unwrap_or_else(|_| Utc::now());

                let stopped_at: Option<DateTime<Utc>> = stopped_at_str.and_then(|s| {
                    s.parse()
                        .or_else(|_| {
                            chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S")
                                .map(|dt| dt.and_utc())
                        })
                        .ok()
                });

                let date = started_at.date_naive();

                // Parse billable times if they exist
                let parse_dt = |s: &str| -> Option<DateTime<Utc>> {
                    s.parse()
                        .or_else(|_| {
                            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
                                .map(|dt| dt.and_utc())
                        })
                        .ok()
                };

                let billable_started_at = billable_start_str.as_ref().and_then(|s| parse_dt(s));
                let billable_stopped_at = billable_stop_str.as_ref().and_then(|s| parse_dt(s));

                let billable_seconds = match (billable_started_at, billable_stopped_at) {
                    (Some(s), Some(e)) => Some(e.signed_duration_since(s).num_seconds()),
                    _ => None,
                };

                let entry = TimeEntryDetail {
                    id,
                    date,
                    started_at,
                    stopped_at,
                    duration_seconds: duration,
                    billable_seconds,
                    billable_started_at,
                    billable_stopped_at,
                };

                tasks_map.entry(task_name).or_default().push(entry);
            }

            if tasks_map.is_empty() {
                continue;
            }

            // Convert to sorted Vec
            let mut tasks: Vec<TaskSummary> = tasks_map
                .into_iter()
                .map(|(task_name, entries)| {
                    let total_seconds = entries.iter().map(|e| e.duration_seconds).sum();
                    let billable_seconds = entries
                        .iter()
                        .map(|e| e.billable_seconds.unwrap_or(e.duration_seconds))
                        .sum();
                    TaskSummary {
                        task_name,
                        total_seconds,
                        billable_seconds,
                        entries,
                    }
                })
                .collect();

            tasks.sort_by(|a, b| a.task_name.cmp(&b.task_name));

            let total_seconds: i64 = tasks.iter().map(|t| t.total_seconds).sum();
            let billable_seconds: i64 = tasks.iter().map(|t| t.billable_seconds).sum();

            projects.push(ProjectSummary {
                project_name,
                total_seconds,
                billable_seconds,
                tasks,
            });
        }

        Ok(projects)
    }

    /// Get the last stopped task (project_id, task_id, project_name, task_name)
    pub fn get_last_stopped_task(&self) -> Result<Option<(i64, i64, String, String)>> {
        let result: SqliteResult<(i64, i64, String, String)> = self.conn.query_row(
            "
            SELECT t.project_id, t.id, p.name, t.name
            FROM time_entries te
            JOIN tasks t ON te.task_id = t.id
            JOIN projects p ON t.project_id = p.id
            WHERE te.stopped_at IS NOT NULL
            ORDER BY te.stopped_at DESC
            LIMIT 1
            ",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        );

        match result {
            Ok(entry) => Ok(Some(entry)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Clear all data from the database
    pub fn clear_all(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            DELETE FROM time_entries;
            DELETE FROM tasks;
            DELETE FROM projects;
            ",
        )?;
        Ok(())
    }
}
