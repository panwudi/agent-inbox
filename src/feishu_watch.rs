//! Follow the existing bot's event journal. Never open another Feishu connection.
use std::collections::{HashSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Result, http, hub, state, token::Token};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    events_file: PathBuf,
    relay: String,
    sender_token_file: PathBuf,
    state_file: PathBuf,
    #[serde(default = "yes")]
    start_at_end: bool,
    routes: Vec<Route>,
}
fn yes() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Route {
    chat: String,
    to: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Snapshot {
    source: PathBuf,
    device: u64,
    inode: u64,
    offset: u64,
    seen: VecDeque<String>,
    pending: Vec<Delivery>,
}

#[derive(Serialize, Deserialize)]
struct Delivery {
    id: String,
    event: String,
    target: String,
    text: String,
    phase: String,
}

fn load(path: &Path) -> Result<Config> {
    let mut config: Config = toml::from_str(&std::fs::read_to_string(path)?)
        .map_err(|_| "invalid feishu-watch config")?;
    config.relay = hub::loopback_endpoint(&config.relay)?;
    config.events_file = hub::relative_to(path, config.events_file);
    config.sender_token_file = hub::relative_to(path, config.sender_token_file);
    config.state_file = hub::relative_to(path, config.state_file);
    let mut chats = HashSet::new();
    if config.routes.is_empty() || config.routes.len() > 256 {
        return Err("feishu-watch needs 1..256 routes".into());
    }
    for route in &config.routes {
        if !route.chat.starts_with("oc_")
            || !route
                .chat
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || !chats.insert(&route.chat)
        {
            return Err("invalid or duplicate Feishu chat route".into());
        }
        hub::qualified(&route.to)?;
    }
    Ok(config)
}

fn message(line: &Value, routes: &[Route]) -> Result<Option<(String, String, String)>> {
    if line["kind"] != "im.message.receive_v1" {
        return Ok(None);
    }
    let event = &line["event"]["event"];
    if event["sender"]["sender_type"] != "user" {
        return Ok(None);
    }
    let msg = &event["message"];
    let Some(route) = routes.iter().find(|r| msg["chat_id"] == r.chat) else {
        return Ok(None);
    };
    let id = msg["message_id"]
        .as_str()
        .ok_or("Feishu message lacks id")?;
    if !id.starts_with("om_") || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Err("invalid Feishu message id".into());
    }
    let content: Value = serde_json::from_str(
        msg["content"]
            .as_str()
            .ok_or("Feishu message lacks content")?,
    )?;
    let body = content["text"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| content.to_string());
    // Sender identity and authority are checked by the receiver with get_message.
    let text = format!(
        "Feishu chat: {}\nfeishu-message: {id}\nMessage type: {}\n\n{body}\n\n[Forwarded by a sender-only service. Do not reply to this relay name. Check the original with feishubot get_message; reply in the source chat when appropriate.]",
        route.chat,
        msg["message_type"].as_str().unwrap_or("unknown")
    );
    crate::validate_message(&route.to, "feishubot", &text)?;
    Ok(Some((id.to_owned(), route.to.clone(), text)))
}

fn ingest(config: &Config, data: &mut Snapshot) -> Result<()> {
    let file = File::open(&config.events_file)?;
    let meta = file.metadata()?;
    if data.device != meta.dev() || data.inode != meta.ino() || data.offset > meta.len() {
        data.device = meta.dev();
        data.inode = meta.ino();
        data.offset = 0;
    }
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(data.offset))?;
    for _ in 0..256 {
        if data.pending.len() >= 256 {
            break;
        }
        let mut bytes = Vec::new();
        // Bound each event, including malformed input; a partial last line waits.
        reader
            .by_ref()
            .take(8 * 1024 * 1024 + 1)
            .read_until(b'\n', &mut bytes)?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err("Feishu event exceeds 8 MiB; cursor retained".into());
        }
        if bytes.last() != Some(&b'\n') {
            break;
        }
        let line: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "invalid Feishu event JSON; cursor retained")?;
        if let Some((event, target, text)) = message(&line, &config.routes)?
            && !data.seen.contains(&event)
        {
            data.pending.push(Delivery {
                id: state::id()?,
                event: event.clone(),
                target,
                text,
                phase: "queued".into(),
            });
            data.seen.push_back(event);
            if data.seen.len() > 4096 {
                data.seen.pop_front();
            }
        }
        data.offset += bytes.len() as u64;
    }
    state::write_json(&config.state_file, data)
}

fn online(config: &Config, token: &Token) -> Result<HashSet<String>> {
    let mut cursor = None;
    let mut found = HashSet::new();
    loop {
        let (status, body) = http::post_response(
            &config.relay,
            "/directory/list",
            token,
            &json!({"cursor":cursor}).to_string(),
        )?;
        http::accepted(status)?;
        let page: Value = serde_json::from_str(&body)?;
        for row in page["agents"].as_array().ok_or("invalid directory page")? {
            if row["host_online"] == true
                && row["inbox_reachable"] == true
                && let Some(name) = row["name"].as_str()
            {
                found.insert(name.to_owned());
            }
        }
        match page["nextCursor"].as_u64() {
            None if page["nextCursor"].is_null() => break,
            Some(next) if cursor.is_none_or(|current| next > current) => cursor = Some(next),
            _ => return Err("invalid directory cursor".into()),
        }
    }
    Ok(found)
}

fn deliver(config: &Config, token: &Token, data: &mut Snapshot) -> Result<()> {
    if !data.pending.iter().any(|d| d.phase == "queued") {
        return Ok(());
    }
    let available = online(config, token)?;
    for index in 0..data.pending.len() {
        let item = &data.pending[index];
        if item.phase != "queued" || !available.contains(&item.target) {
            continue;
        }
        // A crash or lost acknowledgement must never trigger an automatic duplicate.
        data.pending[index].phase = "unconfirmed".into();
        state::write_json(&config.state_file, data)?;
        let item = &data.pending[index];
        match http::post(
            &config.relay,
            &format!("/to/{}", item.target),
            token,
            None,
            &item.text,
            http::SEND_TIMEOUT,
        ) {
            Ok(200 | 202) => {
                eprintln!(
                    "agent-inbox: Feishu delivery {} accepted by {}",
                    item.id, item.target
                );
                data.pending[index].phase = "accepted".into();
            }
            _ => eprintln!(
                "agent-inbox: Feishu delivery {} unconfirmed; inspect state before --retry",
                item.id
            ),
        }
        state::write_json(&config.state_file, data)?;
    }
    data.pending.retain(|d| d.phase != "accepted");
    state::write_json(&config.state_file, data)
}

pub fn run(mut args: impl Iterator<Item = String>) -> Result<()> {
    let mut path = None;
    let mut once = false;
    let mut retry = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => path = Some(PathBuf::from(args.next().ok_or("missing --config value")?)),
            "--once" => once = true,
            "--retry" => retry = Some(args.next().ok_or("missing --retry id")?),
            _ => {
                return Err(
                    "usage: feishu-watch --config <toml> [--once] [--retry <delivery-id>]".into(),
                );
            }
        }
    }
    let config = load(&path.ok_or("feishu-watch requires --config")?)?;
    state::parent(&config.state_file)?;
    let lock = state::open_private(&config.state_file.with_extension("lock"), true)?;
    lock.try_lock()
        .map_err(|_| "another feishu-watch owns this state")?;
    let token = Token::read(&config.sender_token_file)?;
    let mut data: Snapshot = if config.state_file.exists() {
        state::read_json(&config.state_file)?
    } else {
        let meta = std::fs::metadata(&config.events_file)?;
        Snapshot {
            source: config.events_file.clone(),
            device: meta.dev(),
            inode: meta.ino(),
            offset: if config.start_at_end { meta.len() } else { 0 },
            ..Default::default()
        }
    };
    if data.source != config.events_file {
        return Err("event source changed; use a separate state file".into());
    }
    data.pending.retain(|d| d.phase != "accepted");
    if let Some(id) = retry {
        let item = data
            .pending
            .iter_mut()
            .find(|d| d.id == id && d.phase == "unconfirmed")
            .ok_or("no unconfirmed delivery with this id")?;
        item.phase = "queued".into();
    }
    state::write_json(&config.state_file, &data)?;
    eprintln!(
        "agent-inbox: Feishu watcher ready; {} chat routes, {} retained deliveries",
        config.routes.len(),
        data.pending.len()
    );
    loop {
        ingest(&config, &mut data)?;
        if let Err(error) = deliver(&config, &token, &mut data) {
            if once {
                return Err(error);
            }
            eprintln!("agent-inbox: Feishu directory unavailable; queued deliveries retained");
        }
        if once {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU16, Ordering},
    };

    struct Fixture {
        root: PathBuf,
        config: Config,
        running: Arc<AtomicBool>,
        reachable: Arc<AtomicBool>,
        status: Arc<AtomicU16>,
        messages: Arc<Mutex<Vec<String>>>,
        server: Option<std::thread::JoinHandle<()>>,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("inbox-feishu-{}", state::id().unwrap()));
            std::fs::create_dir(&root).unwrap();
            std::fs::write(root.join("events.jsonl"), "").unwrap();
            std::fs::write(
                root.join("sender.token"),
                "feishu-test-private-token-00000000",
            )
            .unwrap();
            std::fs::set_permissions(
                root.join("sender.token"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let relay = listener.local_addr().unwrap().to_string();
            let running = Arc::new(AtomicBool::new(true));
            let reachable = Arc::new(AtomicBool::new(true));
            let status = Arc::new(AtomicU16::new(202));
            let messages = Arc::new(Mutex::new(Vec::new()));
            let (r, online, code, capture) = (
                running.clone(),
                reachable.clone(),
                status.clone(),
                messages.clone(),
            );
            let token = Token::read(&root.join("sender.token")).unwrap();
            let server = std::thread::spawn(move || {
                http::serve_authenticated(listener, r, Arc::new(move |header| token.authorizes(header).then_some(http::SenderIdentity::Header)),
                Arc::new(move |path, text, _, _| {
                    if path == "/directory/list" {
                        http::Response::json(200, json!({"agents":[{"name":"laptop.agent","host_online":true,"inbox_reachable":online.load(Ordering::Relaxed)}],"nextCursor":null}))
                    } else {
                        assert_eq!(path, "/to/laptop.agent");
                        capture.lock().unwrap().push(text.to_owned());
                        http::Response::new(code.load(Ordering::Relaxed), "fake response")
                    }
                })).unwrap();
            });
            std::fs::write(root.join("watch.toml"), format!("events_file='events.jsonl'\nrelay='{relay}'\nsender_token_file='sender.token'\nstate_file='state.json'\nstart_at_end=false\n[[routes]]\nchat='oc_test'\nto='laptop.agent'\n")).unwrap();
            let config = load(&root.join("watch.toml")).unwrap();
            Self {
                root,
                config,
                running,
                reachable,
                status,
                messages,
                server: Some(server),
            }
        }
        fn append(&self, event: &Value) {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&self.config.events_file)
                .unwrap();
            writeln!(f, "{event}").unwrap();
        }
        fn once(&self, extra: &[&str]) {
            let mut args = vec![
                "--config".into(),
                self.root.join("watch.toml").display().to_string(),
                "--once".into(),
            ];
            args.extend(extra.iter().map(|s| (*s).to_owned()));
            run(args.into_iter()).unwrap();
        }
        fn state(&self) -> Snapshot {
            state::read_json(&self.config.state_file).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.running.store(false, Ordering::Release);
            self.server.take().unwrap().join().unwrap();
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }
    fn event(id: &str, sender: &str, chat: &str) -> Value {
        json!({"kind":"im.message.receive_v1","event":{"event":{"sender":{"sender_type":sender},"message":{"chat_id":chat,"message_id":id,"message_type":"text","content":json!({"text":"原文 unchanged\nsecond line"}).to_string()}}}})
    }
    #[test]
    fn routes_only_users_preserves_text_and_deduplicates_across_restart_and_rotation() {
        let f = Fixture::new();
        for ev in [
            event("om_a", "user", "oc_test"),
            event("om_bot", "app", "oc_test"),
            event("om_other", "user", "oc_other"),
            event("om_a", "user", "oc_test"),
        ] {
            f.append(&ev);
        }
        f.once(&[]);
        f.once(&[]);
        let text = f.messages.lock().unwrap();
        assert_eq!(text.len(), 1);
        assert!(text[0].contains("feishu-message: om_a\n"));
        assert!(text[0].contains("原文 unchanged\nsecond line"));
        drop(text);
        std::fs::rename(&f.config.events_file, f.root.join("old.jsonl")).unwrap();
        std::fs::write(&f.config.events_file, "").unwrap();
        f.append(&event("om_a", "user", "oc_test"));
        f.append(&event("om_new", "user", "oc_test"));
        f.once(&[]);
        assert_eq!(f.messages.lock().unwrap().len(), 2);
    }
    #[test]
    fn offline_is_durable_but_uncertain_delivery_requires_explicit_retry() {
        let f = Fixture::new();
        f.reachable.store(false, Ordering::Relaxed);
        f.append(&event("om_a", "user", "oc_test"));
        f.once(&[]);
        assert_eq!(f.state().pending[0].phase, "queued");
        assert!(f.messages.lock().unwrap().is_empty());
        f.reachable.store(true, Ordering::Relaxed);
        f.status.store(503, Ordering::Relaxed);
        f.once(&[]);
        let data = f.state();
        assert_eq!(data.pending[0].phase, "unconfirmed");
        f.once(&[]);
        assert_eq!(f.messages.lock().unwrap().len(), 1);
        f.status.store(200, Ordering::Relaxed);
        f.once(&["--retry", &data.pending[0].id]);
        assert_eq!(f.messages.lock().unwrap().len(), 2);
        assert!(f.state().pending.is_empty());
    }
    #[test]
    fn partial_last_line_waits_and_initial_history_is_skipped_by_default() {
        let f = Fixture::new();
        let text = event("om_a", "user", "oc_test").to_string();
        std::fs::write(&f.config.events_file, &text).unwrap();
        f.once(&[]);
        assert_eq!(f.state().offset, 0);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&f.config.events_file)
            .unwrap();
        file.write_all(b"\n").unwrap();
        f.once(&[]);
        assert_eq!(f.messages.lock().unwrap().len(), 1);
        std::fs::remove_file(&f.config.state_file).unwrap();
        let path = f.root.join("watch.toml");
        let config = std::fs::read_to_string(&path)
            .unwrap()
            .replace("start_at_end=false\n", "");
        std::fs::write(path, config).unwrap();
        f.once(&[]);
        assert_eq!(f.messages.lock().unwrap().len(), 1);
    }
}
