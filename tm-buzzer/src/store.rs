// Reads tm's database for the drums and the timeline on a read-only connection; writes go
// through the tm CLI. The SQL here knows tm's schema (src/db.rs init_schema): projects, tasks
// and time_entries, with times stored as UTC text in "%Y-%m-%d %H:%M:%S", which sorts in time
// order.
use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::{Connection, OpenFlags, params};

const FORMAT: &str = "%Y-%m-%d %H:%M:%S";

pub struct Project {
    pub name: String,
    pub tasks: Vec<String>,
}

pub struct Snapshot {
    // projects and their tasks, most recently used first, then never-used ones by name
    pub projects: Vec<Project>,
}

// one time entry, as the timeline shows it; tm writes times to the second
pub struct Entry {
    // tm's entry id, the one tm amend takes
    pub id: String,
    pub project: String,
    pub task: String,
    pub start: DateTime<Utc>,
    // None while it runs
    pub stop: Option<DateTime<Utc>>,
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
    Ok(Snapshot { projects })
}

// the entries that overlap from..to, in start order; a running one overlaps every span after its
// start. tm does not keep entries apart, so two of them can overlap each other. A row whose time
// does not parse is left out
pub fn entries(conn: &Connection, from: DateTime<Utc>, to: DateTime<Utc>) -> rusqlite::Result<Vec<Entry>> {
    let mut stmt = conn.prepare(
        "
        SELECT te.id, p.name, t.name, te.started_at, te.stopped_at
        FROM time_entries te
        JOIN tasks t ON te.task_id = t.id
        JOIN projects p ON t.project_id = p.id
        WHERE te.started_at < ?2 AND (te.stopped_at IS NULL OR te.stopped_at > ?1)
        ORDER BY te.started_at, te.id
        ",
    )?;
    let rows = stmt.query_map(
        params![from.format(FORMAT).to_string(), to.format(FORMAT).to_string()],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        },
    )?;
    let parse = |s: &str| NaiveDateTime::parse_from_str(s, FORMAT).ok().map(|t| t.and_utc());
    let mut out = Vec::new();
    for row in rows {
        let (id, project, task, start, stop) = row?;
        let Some(start) = parse(&start) else { continue };
        let stop = match stop {
            Some(stop) => match parse(&stop) {
                Some(stop) => Some(stop),
                None => continue,
            },
            None => None,
        };
        out.push(Entry { id, project, task, start, stop });
    }
    Ok(out)
}
