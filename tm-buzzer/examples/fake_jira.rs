// A fake Jira for trying tm-buzzer's Jira tasks: cargo run -p tm-buzzer --example fake_jira
// It answers both search paths over plain HTTP on 127.0.0.1:8089, so jira.toml can say
// url = "http://127.0.0.1:8089" with either kind, and any non-empty token.
// The JQL picks the answer:
// - "project = ABC" gives ABC-1 to ABC-3; any other JQL gives FAKE-1 to FAKE-3
// - a JQL that contains "fail" gives a 500, to see a failed search keep the old issues
// - a request without an Authorization header gives a 401
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

const ADDR: &str = "127.0.0.1:8089";
// summary and assignee of the three issues; the last one has nobody assigned
const ISSUES: [(&str, Option<&str>); 3] = [
    ("Fix the search filters", Some("Ada Lovelace")),
    ("Review the offer import and the price feed after the supplier moved to the new contract", Some("Grace Hopper")),
    ("Write the release notes", None),
];

fn main() {
    let listener = TcpListener::bind(ADDR).expect("bind 127.0.0.1:8089");
    println!("fake jira on http://{ADDR}");
    for stream in listener.incoming().flatten() {
        if let Err(e) = answer(stream) {
            eprintln!("fake jira: {e}");
        }
    }
}

fn answer(mut stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(&stream);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    let mut auth = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("authorization")
        {
            // the scheme only, so the log shows which kind tm-buzzer used
            auth = value.split_whitespace().next().map(str::to_string);
        }
    }

    let target = request.split_whitespace().nth(1).unwrap_or("");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let jql = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("jql="))
        .map(decode)
        .unwrap_or_default();
    println!("{path} auth={} jql={jql:?}", auth.as_deref().unwrap_or("none"));

    let (status, body) = if !matches!(path, "/rest/api/3/search/jql" | "/rest/api/2/search") {
        ("404 Not Found", r#"{"errorMessages":["no such path"]}"#.to_string())
    } else if auth.is_none() {
        ("401 Unauthorized", r#"{"errorMessages":["no Authorization header"]}"#.to_string())
    } else if jql.contains("fail") {
        ("500 Internal Server Error", r#"{"errorMessages":["asked to fail"]}"#.to_string())
    } else {
        ("200 OK", issues(&prefix(&jql)))
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

// the key prefix of "project = ABC", else FAKE
fn prefix(jql: &str) -> String {
    jql.split_once("project")
        .and_then(|(_, rest)| rest.trim_start().strip_prefix('='))
        .map(|rest| {
            rest.trim_start()
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
        })
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "FAKE".to_string())
}

fn issues(prefix: &str) -> String {
    let issues: Vec<String> = ISSUES
        .iter()
        .enumerate()
        .map(|(i, (summary, assignee))| {
            let assignee = match assignee {
                Some(name) => format!(r#"{{"displayName":"{name}"}}"#),
                None => "null".to_string(),
            };
            format!(
                r#"{{"id":"{}","key":"{prefix}-{}","fields":{{"summary":"{summary}","assignee":{assignee}}}}}"#,
                i + 1,
                i + 1
            )
        })
        .collect();
    format!(r#"{{"issues":[{}]}}"#, issues.join(","))
}

// percent-decoding of a query value, with + as a space
fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => match value.get(i + 1..i + 3).and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                Some(b) => {
                    out.push(b);
                    i += 2;
                }
                None => out.push(b'%'),
            },
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
