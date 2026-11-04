// ~/.config/tm/buzzer.toml; a missing file is the default, an empty configuration:
//
//   ignore = ["meetings", "BOP-2892"]   # project or task names the lists skip, matched whole
//
//   [jira]                              # optional, see jira.rs
//   url = "https://company.atlassian.net"
//   kind = "cloud"
//   user = "me@example.com"
//   token = "..."                       # keep the file private: chmod 600
//
//   [[jira.project]]
//   name = "review"
//   jql = "assignee = currentUser() AND status = 'In Review'"
use std::path::PathBuf;

use serde::Deserialize;

use crate::jira;

#[derive(Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub ignore: Vec<String>,
    pub jira: Option<jira::Config>,
}

// an unreadable or malformed file gives the default, with the reason on stderr
pub fn load() -> Config {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let path = PathBuf::from(home).join(".config/tm/buzzer.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Config::default(),
        Err(e) => {
            eprintln!("config: {}: {e}", path.display());
            return Config::default();
        }
    };
    toml::from_str(&text).unwrap_or_else(|e| {
        eprintln!("config: {}: {e}", path.display());
        Config::default()
    })
}
