// Jira issues offered as extra tasks; the search follows git-pr's src/jira.rs.
// The [jira] part of buzzer.toml lists tm projects, each with a JQL; every issue the JQL finds
// becomes a task of that project, named by its key. tm sees only the names, so tm holds no Jira
// code. The token comes from the environment variable that `token_env` names, never from the file.
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use ureq::tls::{RootCerts, TlsConfig};

const MAX_RESULTS: &str = "50";
const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    // Basic auth with `user` and the token, search at /rest/api/3/search/jql
    Cloud,
    // Bearer personal access token, search at /rest/api/2/search
    Datacenter,
}

#[derive(Deserialize)]
pub struct Config {
    // the Jira address, e.g. "https://company.atlassian.net"
    url: String,
    kind: Kind,
    // the account email, Cloud only
    user: Option<String>,
    #[serde(default = "token_env")]
    token_env: String,
    #[serde(default)]
    project: Vec<Source>,
}

fn token_env() -> String {
    "JIRA_TOKEN".to_string()
}

#[derive(Deserialize)]
struct Source {
    // the tm project the issues go under
    name: String,
    jql: String,
}

#[derive(Clone)]
pub struct Task {
    pub project: String,
    pub key: String,
    pub summary: String,
    pub assignee: Option<String>,
}

struct Client {
    search_url: String,
    // the `Authorization` header value
    auth: String,
    agent: ureq::Agent,
}

// the search thread: it searches once at launch, then once for each refresh, one search at a time
pub struct Watch {
    refresh: mpsc::Sender<()>,
    answers: mpsc::Receiver<Vec<Task>>,
}

impl Watch {
    // asks for a search; while one runs, the refresh waits for its answer and starts none
    pub fn refresh(&self) {
        self.refresh.send(()).ok();
    }

    // the newest answer that arrived since the last call
    pub fn latest(&self) -> Option<Vec<Task>> {
        self.answers.try_iter().last()
    }
}

// the issues of every project, after each search; None when the token is missing or the
// configuration unusable, with the reason on stderr
pub fn watch(config: Config) -> Option<Watch> {
    let client = Client::new(&config)
        .map_err(|e| eprintln!("jira: {e}"))
        .ok()?;
    let (tx, rx) = mpsc::channel();
    let (refresh, refreshes) = mpsc::channel::<()>();
    thread::spawn(move || {
        // the last good answer of each project; a failed search keeps its project's old issues
        let mut found: Vec<Vec<Task>> = config.project.iter().map(|_| Vec::new()).collect();
        loop {
            for (source, tasks) in config.project.iter().zip(found.iter_mut()) {
                match client.search(&source.jql) {
                    Ok(issues) => {
                        *tasks = issues
                            .into_iter()
                            .map(|issue| Task {
                                project: source.name.clone(),
                                key: issue.key,
                                summary: issue.fields.summary.unwrap_or_default(),
                                assignee: issue.fields.assignee.map(|u| u.display_name),
                            })
                            .collect()
                    }
                    Err(e) => eprintln!("jira: {}: {e}", source.name),
                }
            }
            if tx.send(found.iter().flatten().cloned().collect()).is_err() {
                return;
            }
            // a refresh asked during the search joins it: the answer just sent serves it
            refreshes.try_iter().count();
            // waits for a refresh; the app dropping its Watch ends the thread
            if refreshes.recv().is_err() {
                return;
            }
        }
    });
    Some(Watch {
        refresh,
        answers: rx,
    })
}

impl Client {
    fn new(config: &Config) -> Result<Self, String> {
        let token = std::env::var(&config.token_env)
            .ok()
            .filter(|t| !t.is_empty())
            .ok_or_else(|| format!("set {}", config.token_env))?;
        let base = config.url.trim_end_matches('/');
        let (path, auth) = match config.kind {
            Kind::Cloud => {
                let user = config.user.as_deref().ok_or("set `user` for a cloud Jira")?;
                let basic = STANDARD.encode(format!("{user}:{token}"));
                ("rest/api/3/search/jql", format!("Basic {basic}"))
            }
            Kind::Datacenter => ("rest/api/2/search", format!("Bearer {token}")),
        };
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_global(Some(TIMEOUT))
                .tls_config(
                    TlsConfig::builder()
                        .root_certs(RootCerts::PlatformVerifier)
                        .build(),
                )
                .build(),
        );
        Ok(Self {
            search_url: format!("{base}/{path}"),
            auth,
            agent,
        })
    }

    fn search(&self, jql: &str) -> Result<Vec<Issue>, ureq::Error> {
        let response: SearchResponse = self
            .agent
            .get(&self.search_url)
            .header("Authorization", &self.auth)
            .header("Accept", "application/json")
            .query("jql", jql)
            .query("fields", "summary,assignee")
            .query("maxResults", MAX_RESULTS)
            .call()?
            .body_mut()
            .read_json()?;
        Ok(response.issues)
    }
}

// the search response of both API versions, reduced to the fields read here
#[derive(Deserialize)]
struct SearchResponse {
    issues: Vec<Issue>,
}

#[derive(Deserialize)]
struct Issue {
    key: String,
    fields: IssueFields,
}

#[derive(Deserialize)]
struct IssueFields {
    summary: Option<String>,
    // null when nobody is assigned
    assignee: Option<User>,
}

#[derive(Deserialize)]
struct User {
    #[serde(rename = "displayName")]
    display_name: String,
}
