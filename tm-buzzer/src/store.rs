// Reads tm's database for the drums on a read-only connection; writes go through the tm CLI.
// The SQL here knows tm's schema (src/db.rs init_schema): projects, tasks and time_entries,
// with times stored as UTC text in "%Y-%m-%d %H:%M:%S", which sorts in time order.
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Local, NaiveDateTime, Utc};
use rusqlite::{Connection, OpenFlags, params};

// tasks offered on the tracking drum, most recently started first
const RECENT: usize = 6;
const FORMAT: &str = "%Y-%m-%d %H:%M:%S";

pub struct Project {
    pub name: String,
    pub tasks: Vec<String>,
}

pub struct Snapshot {
    // projects and their tasks, most recently used first, then never-used ones by name
    pub projects: Vec<Project>,
    // (project, task) indices into projects, most recently started first
    pub recent: Vec<(usize, usize)>,
    // the running entry as indices into projects, and when it started
    pub running: Option<(usize, usize, DateTime<Utc>)>,
    // time of the stopped entries that started today, local time
    pub logged: Duration,
}

// tm's database file, the path tm::db opens; read-only, so a missing file is an error
pub fn open() -> rusqlite::Result<Connection> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    Connection::open_with_flags(
        PathBuf::from(home).join(".config/tm/data.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
}

pub fn load(conn: &Connection) -> rusqlite::Result<Snapshot> {
    // every project with every task, and the latest start of each task; a project without tasks
    // comes back once with a NULL task
    let mut stmt = conn.prepare(
        "
        SELECT p.name, t.name, MAX(te.started_at)
        FROM projects p
        LEFT JOIN tasks t ON t.project_id = p.id
        LEFT JOIN time_entries te ON te.task_id = t.id
        GROUP BY p.id, t.id
        ",
    )?;
    let mut used: Vec<(String, Option<String>, Vec<(String, Option<String>)>)> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    for row in rows {
        let (project, task, last) = row?;
        let i = *at.entry(project.clone()).or_insert_with(|| {
            used.push((project, None, Vec::new()));
            used.len() - 1
        });
        let entry = &mut used[i];
        if last > entry.1 {
            entry.1 = last.clone();
        }
        if let Some(task) = task {
            entry.2.push((task, last));
        }
    }
    // latest first; None sorts below every time, and names break ties
    let by_use = |a: &(String, Option<String>), b: &(String, Option<String>)| {
        b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0))
    };
    used.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let projects: Vec<Project> = used
        .into_iter()
        .map(|(name, _, mut tasks)| {
            tasks.sort_by(by_use);
            Project {
                name,
                tasks: tasks.into_iter().map(|(task, _)| task).collect(),
            }
        })
        .collect();
    let find = |project: &str, task: &str| {
        let p = projects.iter().position(|p| p.name == project)?;
        let t = projects[p].tasks.iter().position(|t| t == task)?;
        Some((p, t))
    };

    let mut stmt = conn.prepare(
        "
        SELECT p.name, t.name
        FROM time_entries te
        JOIN tasks t ON te.task_id = t.id
        JOIN projects p ON t.project_id = p.id
        GROUP BY t.id
        ORDER BY MAX(te.started_at) DESC
        LIMIT ?1
        ",
    )?;
    let recent = stmt
        .query_map(params![RECENT as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .filter_map(|(project, task)| find(project, task))
        .collect();

    let running = conn
        .query_row(
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
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            e => Err(e),
        })?
        .and_then(|(project, task, started)| {
            let (p, t) = find(&project, &task)?;
            let started = NaiveDateTime::parse_from_str(&started, FORMAT).ok()?.and_utc();
            Some((p, t, started))
        });

    // an entry that crossed midnight counts whole on the day it started
    let midnight = Local::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|m| m.and_local_timezone(Local).earliest())
        .map(|m| m.with_timezone(&Utc).format(FORMAT).to_string())
        .unwrap_or_default();
    let seconds: i64 = conn.query_row(
        "
        SELECT COALESCE(SUM(strftime('%s', stopped_at) - strftime('%s', started_at)), 0)
        FROM time_entries
        WHERE stopped_at IS NOT NULL AND started_at >= ?1
        ",
        params![midnight],
        |row| row.get(0),
    )?;

    Ok(Snapshot {
        projects,
        recent,
        running,
        logged: Duration::from_secs(seconds.max(0) as u64),
    })
}
