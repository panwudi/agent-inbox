#![cfg(unix)]
use agent_inbox::{http, token::Token};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

static NEXT: AtomicU64 = AtomicU64::new(0);
const AHOST: &str = "alpha-host-private-token-000000000";
const BHOST: &str = "beta-host-private-token-0000000000";
const ASEND: &str = "alice-local-sender-token-00000000";
const BSEND: &str = "bob-local-sender-token-0000000000";
const AIN: &str = "alice-inbox-private-token-0000000";
const BIN: &str = "bob-inbox-private-token-000000000";
const CSEND: &str = "charlie-local-sender-token-00000";
const CIN: &str = "charlie-inbox-private-token-00000";
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let root = PathBuf::from(format!(
            "/tmp/agent-inbox-relay-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let temp = Self(root);
        for (name, secret) in [
            ("alpha.host", AHOST),
            ("beta.host", BHOST),
            ("alice.send", ASEND),
            ("bob.send", BSEND),
            ("alice.in", AIN),
            ("bob.in", BIN),
            ("charlie.send", CSEND),
            ("charlie.in", CIN),
        ] {
            std::fs::write(temp.0.join(name), secret).unwrap();
            std::fs::set_permissions(temp.0.join(name), std::fs::Permissions::from_mode(0o600))
                .unwrap();
        }
        temp
    }
    fn relay_config(&self, name: &str, hub: &str, inbox: &str) -> PathBuf {
        let agent = if name == "alpha" { "alice" } else { "bob" };
        let config = format!(
            "host='{name}'\nhub='{hub}'\nhost_token_file='{name}.host'\njournal='{name}.md'\ncontrol_socket='control/{name}.sock'\ndirectory_report_secs=1\n[[agents]]\nname='{agent}'\ndescription='Owns {agent} test work'\ninbox='{inbox}'\ntoken_file='{agent}.in'\nsender_token_file='{agent}.send'\n"
        );
        let path = self.0.join(format!("{name}.toml"));
        std::fs::write(&path, config).unwrap();
        path
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Process {
    child: Child,
    address: String,
    errors: Arc<Mutex<String>>,
}
impl Process {
    fn start(args: Vec<String>) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_agent-inbox"))
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        let (ready, received) = mpsc::channel();
        let errors = Arc::new(Mutex::new(String::new()));
        let capture = Arc::clone(&errors);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                capture.lock().unwrap().push_str(&format!("{line}\n"));
                if let Some(address) = line.strip_prefix("agent-inbox: listening on http://") {
                    let _ = ready.send(address.trim_end_matches('/').to_owned());
                }
            }
        });
        let address = received
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| {
                let _ = child.kill();
                let _ = child.wait();
                panic!("process did not listen");
            });
        Self {
            child,
            address,
            errors,
        }
    }
    fn relay(temp: &Temp, config: &Path, listen: &str) -> Self {
        Self::start(vec![
            "relay".into(),
            "--listen".into(),
            listen.into(),
            "--config".into(),
            config.display().to_string(),
            "--state-dir".into(),
            temp.0.join("state").display().to_string(),
            "--no-tunnel".into(),
        ])
    }
    fn stop(&mut self) {
        signal(self.child.id(), "TERM");
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            signal(self.child.id(), "TERM");
            let _ = self.child.wait();
        }
    }
}
fn signal(pid: u32, name: &str) {
    let _ = Command::new("kill")
        .args([&format!("-{name}"), &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}
struct Inbox {
    address: String,
    received: mpsc::Receiver<(String, String)>,
    running: Arc<AtomicBool>,
    server: Option<std::thread::JoinHandle<agent_inbox::Result<()>>>,
}
impl Inbox {
    fn new(temp: &Temp, token: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let running = Arc::new(AtomicBool::new(true));
        let control = Arc::clone(&running);
        let token = Token::read(&temp.0.join(token)).unwrap();
        let (sent, received) = mpsc::channel();
        let handler = Arc::new(move |text: &str, from: Option<&str>| {
            sent.send((text.to_owned(), from.unwrap_or("").to_owned()))
                .unwrap();
            http::Response::new(202, "fake queued")
        });
        let server = std::thread::spawn(move || http::serve(listener, token, control, handler));
        Self {
            address,
            received,
            running,
            server: Some(server),
        }
    }
    fn message(&self) -> (String, String) {
        self.received.recv_timeout(Duration::from_secs(3)).unwrap()
    }
}
impl Drop for Inbox {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        self.server.take().unwrap().join().unwrap().unwrap();
    }
}
fn post(addr: &str, path: &str, secret: &str, sender: &str, text: &str) -> u16 {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    write!(stream,"POST {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {secret}\r\nX-Agent-From: {sender}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",text.len()).unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    line.split_whitespace().nth(1).unwrap().parse().unwrap()
}
fn hub(temp: &Temp, listen: &str, a: &str, b: &str) -> Process {
    let config = format!(
        "journal='hub.md'\n[[hosts]]\nname='alpha'\nrelay='{a}'\ntoken_file='alpha.host'\n[[hosts]]\nname='beta'\nrelay='{b}'\ntoken_file='beta.host'\n"
    );
    let path = temp.0.join("hub.toml");
    std::fs::write(&path, config).unwrap();
    Process::start(vec![
        "hub".into(),
        "--listen".into(),
        listen.into(),
        "--config".into(),
        path.display().to_string(),
    ])
}

#[test]
fn cross_host_delivery_uses_host_tokens_and_preserves_qualified_sender() {
    let temp = Temp::new();
    let a = Inbox::new(&temp, "alice.in");
    let b = Inbox::new(&temp, "bob.in");
    let reserved = TcpListener::bind("127.0.0.1:0").unwrap();
    let central = reserved.local_addr().unwrap().to_string();
    let ar = Process::relay(
        &temp,
        &temp.relay_config("alpha", &central, &a.address),
        "0",
    );
    let br = Process::relay(&temp, &temp.relay_config("beta", &central, &b.address), "0");
    drop(reserved);
    let hub = hub(&temp, &central, &ar.address, &br.address);
    assert_eq!(
        post(
            &ar.address,
            "/to/beta.bob",
            ASEND,
            "beta.forged",
            "alpha to beta"
        ),
        202
    );
    assert_eq!(b.message(), ("alpha to beta".into(), "alpha.alice".into()));
    assert_eq!(
        post(
            &br.address,
            "/to/alpha.alice",
            BSEND,
            "alpha.forged",
            "beta to alpha"
        ),
        202
    );
    assert_eq!(a.message(), ("beta to alpha".into(), "beta.bob".into()));
    assert_eq!(
        post(
            &hub.address,
            "/to/beta.bob",
            AHOST,
            "beta.forged",
            "wrong prefix"
        ),
        403
    );
    assert_eq!(
        post(
            &hub.address,
            "/to/beta.bob",
            AHOST,
            "alphax.alice",
            "prefix lookalike"
        ),
        403
    );
    assert_eq!(
        post(
            &hub.address,
            "/to/beta.bob",
            AHOST,
            "alpha.alice.extra",
            "malformed sender"
        ),
        400
    );
    assert_eq!(
        post(
            &hub.address,
            "/to/beta.bob",
            ASEND,
            "alpha.alice",
            "local token stays local"
        ),
        401
    );
    assert!(b.received.recv_timeout(Duration::from_millis(100)).is_err());
    for path in ["hub.md", "alpha.md", "beta.md"] {
        let journal = std::fs::read_to_string(temp.0.join(path)).unwrap();
        for token in [AHOST, BHOST, ASEND, BSEND, AIN, BIN] {
            assert!(!journal.contains(token));
        }
    }
}

#[test]
fn sender_only_agents_send_locally_and_across_hosts_without_becoming_inboxes() {
    let temp = Temp::new();
    let a = Inbox::new(&temp, "alice.in");
    let b = Inbox::new(&temp, "bob.in");
    let reserved = TcpListener::bind("127.0.0.1:0").unwrap();
    let central = reserved.local_addr().unwrap().to_string();
    let config = temp.relay_config("alpha", &central, &a.address);
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str("\n[[agents]]\nname='bridge'\ndescription='Forwards chat messages'\nsender_token_file='charlie.send'\n");
    std::fs::write(&config, text).unwrap();
    let ar = Process::relay(&temp, &config, "0");
    let br = Process::relay(&temp, &temp.relay_config("beta", &central, &b.address), "0");
    drop(reserved);
    let _hub = hub(&temp, &central, &ar.address, &br.address);
    assert_eq!(
        post(&ar.address, "/to/alpha.alice", CSEND, "spoofed", "local"),
        202
    );
    assert_eq!(a.message(), ("local".into(), "alpha.bridge".into()));
    assert_eq!(
        post(&ar.address, "/to/beta.bob", CSEND, "spoofed", "remote"),
        202
    );
    assert_eq!(b.message(), ("remote".into(), "alpha.bridge".into()));
    assert_eq!(
        post(&ar.address, "/to/alpha.bridge", ASEND, "", "reply"),
        405
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let page = directory_page(&ar.address, &temp.0.join("charlie.send"));
        let rows = page["agents"].as_array().unwrap();
        assert!(!rows.iter().any(|r| r["name"] == "alpha.bridge"));
        if rows.iter().any(|r| r["name"] == "alpha.alice") {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn chat_registration_uses_relay_authenticated_identity_and_rejects_cross_host_spoofing() {
    let temp = Temp::new();
    let a = Inbox::new(&temp, "alice.in");
    let b = Inbox::new(&temp, "bob.in");
    let reserve = TcpListener::bind("127.0.0.1:0").unwrap();
    let central = reserve.local_addr().unwrap().to_string();
    let ar = Process::relay(
        &temp,
        &temp.relay_config("alpha", &central, &a.address),
        "0",
    );
    let br = Process::relay(&temp, &temp.relay_config("beta", &central, &b.address), "0");
    drop(reserve);
    let hub = hub(&temp, &central, &ar.address, &br.address);
    let body = r#"{"chat":"oc_team"}"#;
    assert_eq!(
        post(&ar.address, "/chats/register", ASEND, "beta.bob", body),
        200
    );
    let (code, body) = http::post_response(
        &br.address,
        "/chats/list",
        &Token::read(&temp.0.join("bob.send")).unwrap(),
        "{}",
    )
    .unwrap();
    assert_eq!(code, 200);
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        page["routes"],
        serde_json::json!([{"chat":"oc_team","agent":"alpha.alice"}])
    );
    assert_eq!(
        post(
            &br.address,
            "/chats/register",
            BSEND,
            "alpha.alice",
            r#"{"chat":"oc_team"}"#
        ),
        409
    );
    assert_eq!(
        post(
            &ar.address,
            "/chats/register",
            ASEND,
            "",
            r#"{"chat":"oc_fake","agent":"beta.bob"}"#
        ),
        400
    );
    assert_eq!(
        post(
            &ar.address,
            "/chats/register",
            AHOST,
            "beta.bob",
            r#"{"chat":"oc_inbound"}"#
        ),
        403
    );
    assert_eq!(
        post(
            &hub.address,
            "/chats/register",
            AHOST,
            "beta.bob",
            r#"{"chat":"oc_outside"}"#
        ),
        403
    );
    assert_eq!(
        post(
            &ar.address,
            "/chats/remove",
            ASEND,
            "",
            r#"{"chat":"oc_team"}"#
        ),
        200
    );
}

#[test]
fn same_host_short_circuit_and_inbound_trust_do_not_bounce_to_hub() {
    let temp = Temp::new();
    let a = Inbox::new(&temp, "alice.in");
    let b = Inbox::new(&temp, "charlie.in");
    let dummy = TcpListener::bind("127.0.0.1:0").unwrap();
    let config = temp.relay_config(
        "alpha",
        &dummy.local_addr().unwrap().to_string(),
        &a.address,
    );
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str(&format!("\n[[agents]]\nname='charlie'\ndescription='Owns Charlie work'\ninbox='{}'\ntoken_file='charlie.in'\nsender_token_file='charlie.send'\n",b.address));
    std::fs::write(&config, text).unwrap();
    let relay = Process::relay(&temp, &config, "0");
    assert_eq!(
        post(
            &relay.address,
            "/to/alpha.charlie",
            ASEND,
            "beta.claim",
            "local shortcut"
        ),
        202
    );
    assert_eq!(b.message(), ("local shortcut".into(), "alpha.alice".into()));
    assert_eq!(
        post(
            &relay.address,
            "/to/alpha.alice",
            AHOST,
            "beta.bob",
            "hub inbound"
        ),
        202
    );
    assert_eq!(a.message(), ("hub inbound".into(), "beta.bob".into()));
    assert_eq!(
        post(
            &relay.address,
            "/to/alpha.alice",
            AHOST,
            "legacy-main",
            "legacy inbound"
        ),
        202
    );
    assert_eq!(a.message(), ("legacy inbound".into(), "legacy-main".into()));
    assert_eq!(
        post(
            &relay.address,
            "/to/beta.bob",
            AHOST,
            "beta.bob",
            "bounce forbidden"
        ),
        403
    );
    assert_eq!(
        post(
            &relay.address,
            "/to/alpha.unknown",
            ASEND,
            "",
            "unknown local"
        ),
        404
    );
}

#[test]
fn adding_agent_changes_only_the_local_relay_configuration() {
    let temp = Temp::new();
    let a = Inbox::new(&temp, "alice.in");
    let b = Inbox::new(&temp, "bob.in");
    let c = Inbox::new(&temp, "charlie.in");
    let reserved = TcpListener::bind("127.0.0.1:0").unwrap();
    let central = reserved.local_addr().unwrap().to_string();
    let config = temp.relay_config("alpha", &central, &a.address);
    let ar = Process::relay(&temp, &config, "0");
    let br = Process::relay(&temp, &temp.relay_config("beta", &central, &b.address), "0");
    drop(reserved);
    let hub = hub(&temp, &central, &ar.address, &br.address);
    let before = std::fs::read(temp.0.join("hub.toml")).unwrap();
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str(&format!("\n[[agents]]\nname='charlie'\ndescription='Owns Charlie work'\ninbox='{}'\ntoken_file='charlie.in'\nsender_token_file='charlie.send'\n",c.address));
    std::fs::write(&config, text).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let page = directory_page(&ar.address, &temp.0.join("alice.send"));
        if page["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["name"] == "alpha.charlie")
        {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        post(
            &br.address,
            "/to/alpha.charlie",
            BSEND,
            "",
            "new local agent"
        ),
        202
    );
    assert_eq!(c.message(), ("new local agent".into(), "beta.bob".into()));
    assert_eq!(std::fs::read(temp.0.join("hub.toml")).unwrap(), before);
    assert!(
        Command::new("kill")
            .args(["-0", &hub.child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn legacy_flat_hub_routes_remain_usable_during_migration() {
    let temp = Temp::new();
    let inbox = Inbox::new(&temp, "alice.in");
    let path = temp.0.join("legacy.toml");
    std::fs::write(&path,format!("journal='legacy.md'\n[[senders]]\nname='old-sender'\ntoken_file='alice.send'\n[[peers]]\nname='old-agent'\ninbox='{}'\ntoken_file='alice.in'\n",inbox.address)).unwrap();
    let hub = Process::start(vec![
        "hub".into(),
        "--listen".into(),
        "0".into(),
        "--config".into(),
        path.display().to_string(),
    ]);
    assert_eq!(
        post(
            &hub.address,
            "/to/old-agent",
            ASEND,
            "forged",
            "old wire form"
        ),
        202
    );
    assert_eq!(
        inbox.message(),
        ("old wire form".into(), "old-sender".into())
    );
}

#[test]
fn mixed_legacy_sender_and_host_relay_deliver_both_directions_and_ack_reports() {
    let temp = Temp::new();
    let old = Inbox::new(&temp, "bob.in");
    let new = Inbox::new(&temp, "alice.in");
    let reserved = TcpListener::bind("127.0.0.1:0").unwrap();
    let central = reserved.local_addr().unwrap().to_string();
    let config = temp.0.join("mixed-relay.toml");
    std::fs::write(&config, format!("host='laptop1'\nhub='{central}'\nhost_token_file='alpha.host'\njournal='relay.md'\ncontrol_socket='control/laptop1.sock'\ndirectory_report_secs=1\n[[agents]]\nname='main-review'\ndescription='Main review session'\ninbox='{}'\ntoken_file='alice.in'\nsender_token_file='alice.send'\n", new.address)).unwrap();
    let relay = Process::relay(&temp, &config, "0");
    let config = temp.0.join("mixed-hub.toml");
    std::fs::write(&config, format!("journal='mixed-hub.md'\n[[senders]]\nname='main-review'\ntoken_file='bob.send'\n[[peers]]\nname='main-review'\ninbox='{}'\ntoken_file='bob.in'\n[[hosts]]\nname='laptop1'\nrelay='{}'\ntoken_file='alpha.host'\n", old.address, relay.address)).unwrap();
    drop(reserved);
    let hub = Process::start(vec![
        "hub".into(),
        "--listen".into(),
        central,
        "--config".into(),
        config.display().to_string(),
    ]);
    assert_eq!(
        post(
            &hub.address,
            "/to/laptop1.main-review",
            BSEND,
            "ignored-forged",
            "legacy to new"
        ),
        202
    );
    assert_eq!(
        new.message(),
        ("legacy to new".into(), "main-review".into())
    );
    assert_eq!(
        post(
            &relay.address,
            "/to/main-review",
            ASEND,
            "ignored-forged",
            "new to legacy"
        ),
        202
    );
    assert_eq!(
        old.message(),
        ("new to legacy".into(), "laptop1.main-review".into())
    );
    let report=serde_json::json!({"agents":[{"name":"laptop1.main-review","description":"Main review session","online":true}]}).to_string();
    assert_eq!(
        post(
            &hub.address,
            "/directory/report",
            AHOST,
            "laptop1.relay",
            &report
        ),
        200
    );
    assert_eq!(
        post(&hub.address, "/directory/report", AHOST, "laptop1", &report),
        403
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let page = directory_page(&relay.address, &temp.0.join("alice.send"));
        if page["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["name"] == "laptop1.main-review" && row["status"] == "online")
        {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn relay_reports_qualified_identity_and_logs_only_known_status_reason() {
    let temp = Temp::new();
    let inbox = Inbox::new(&temp, "alice.in");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let running = Arc::new(AtomicBool::new(true));
    let control = Arc::clone(&running);
    let token = Token::read(&temp.0.join("alpha.host")).unwrap();
    let authenticate = Arc::new(move |header: &[u8]| {
        token
            .authorizes(header)
            .then_some(http::SenderIdentity::HostPrefix("alpha".into()))
    });
    let (sent, received) = mpsc::channel();
    let rejected = Arc::new(AtomicBool::new(true));
    let rejecting = Arc::clone(&rejected);
    let handler = Arc::new(
        move |path: &str, text: &str, from: Option<&str>, origin: http::Origin| {
            assert_eq!(path, "/directory/report");
            assert!(origin == http::Origin::Host);
            sent.send((text.to_owned(), from.unwrap().to_owned()))
                .unwrap();
            if rejecting.load(Ordering::Relaxed) {
                http::Response::new(400, "invalid or out-of-prefix directory report")
            } else {
                http::Response::new(200, "directory report saved")
            }
        },
    );
    let server = std::thread::spawn(move || {
        http::serve_authenticated(listener, control, authenticate, handler)
    });
    let central = Inbox {
        address,
        received,
        running,
        server: Some(server),
    };
    let relay = Process::relay(
        &temp,
        &temp.relay_config("alpha", &central.address, &inbox.address),
        "0",
    );
    let (body, from) = central.message();
    assert_eq!(from, "alpha.relay");
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["agents"][0]["name"], "alpha.alice");
    let deadline = Instant::now() + Duration::from_secs(3);
    while !relay
        .errors
        .lock()
        .unwrap()
        .contains("HTTP 400: invalid or out-of-prefix directory report")
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(30));
    }
    rejected.store(false, Ordering::Relaxed);
    central.message();
    assert!(!relay.errors.lock().unwrap().contains(AHOST));
    assert_eq!(
        http::response_reason("arbitrary echoed private contents"),
        "unrecognized response (body omitted)"
    );
}

#[test]
fn directory_waits_quietly_once_while_local_hub_forward_is_down() {
    let temp = Temp::new();
    let inbox = Inbox::new(&temp, "alice.in");
    let reserve = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reserve.local_addr().unwrap().to_string();
    drop(reserve);
    let relay = Process::relay(
        &temp,
        &temp.relay_config("alpha", &address, &inbox.address),
        "0",
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while !relay
        .errors
        .lock()
        .unwrap()
        .contains("directory waiting for tunnel")
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(30));
    }
    std::thread::sleep(Duration::from_millis(2200));
    let errors = relay.errors.lock().unwrap();
    assert_eq!(errors.matches("directory waiting for tunnel").count(), 1);
    assert!(!errors.contains("report unconfirmed"));
}

#[test]
fn host_relay_instance_lock_and_loopback_binding_are_enforced() {
    let temp = Temp::new();
    let inbox = Inbox::new(&temp, "alice.in");
    let dummy = TcpListener::bind("127.0.0.1:0").unwrap();
    let config = temp.relay_config(
        "alpha",
        &dummy.local_addr().unwrap().to_string(),
        &inbox.address,
    );
    let _one = Process::relay(&temp, &config, "0");
    for listen in ["0", "0.0.0.0:0"] {
        let output = Command::new(env!("CARGO_BIN_EXE_agent-inbox"))
            .args(["relay", "--listen", listen, "--config"])
            .arg(&config)
            .arg("--state-dir")
            .arg(temp.0.join("state"))
            .arg("--no-tunnel")
            .output()
            .unwrap();
        assert!(!output.status.success());
        let err = String::from_utf8_lossy(&output.stderr);
        assert!(err.contains(if listen == "0" {
            "already running"
        } else {
            "loopback"
        }));
        assert!(!err.contains(AHOST));
    }
}

#[test]
fn host_and_local_sender_credentials_cannot_be_reused() {
    let temp = Temp::new();
    let inbox = Inbox::new(&temp, "alice.in");
    let dummy = TcpListener::bind("127.0.0.1:0").unwrap();
    let config = temp.relay_config(
        "alpha",
        &dummy.local_addr().unwrap().to_string(),
        &inbox.address,
    );
    let text = std::fs::read_to_string(&config).unwrap().replace(
        "sender_token_file='alice.send'",
        "sender_token_file='alpha.host'",
    );
    std::fs::write(&config, text).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_agent-inbox"))
        .args(["relay", "--listen", "0", "--config"])
        .arg(config)
        .arg("--no-tunnel")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains("independent"));
    assert!(!err.contains(AHOST));
}

fn directory_page(addr: &str, token_file: &Path) -> serde_json::Value {
    let token = Token::read(token_file).unwrap();
    let (status, body) = http::post_response(addr, "/directory/list", &token, "{}").unwrap();
    assert_eq!(status, 200);
    serde_json::from_str(&body).unwrap()
}

#[test]
fn directory_is_prefix_scoped_authenticated_and_rebuilt_offline_on_restart() {
    let temp = Temp::new();
    let a = Inbox::new(&temp, "alice.in");
    let b = Inbox::new(&temp, "bob.in");
    let reserve = TcpListener::bind("127.0.0.1:0").unwrap();
    let central = reserve.local_addr().unwrap().to_string();
    let ac = temp.relay_config("alpha", &central, &a.address);
    let bc = temp.relay_config("beta", &central, &b.address);
    let mut ar = Process::relay(&temp, &ac, "0");
    let mut br = Process::relay(&temp, &bc, "0");
    drop(reserve);
    let mut central_process = hub(&temp, &central, &ar.address, &br.address);
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let page = directory_page(&ar.address, &temp.0.join("alice.send"));
        if page["agents"].as_array().unwrap().len() == 2 {
            assert!(
                page["agents"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|row| row["status"] == "online")
            );
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(100));
    }
    let output = Command::new(env!("CARGO_BIN_EXE_agent-inbox"))
        .args(["who", "--hub", &ar.address, "--token-file"])
        .arg(temp.0.join("alice.send"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("beta.bob"));
    assert!(text.contains("Owns bob test work"));
    assert!(!text.contains(BHOST));
    assert_eq!(post(&ar.address, "/directory/list", "wrong", "", "{}"), 401);
    let forged=serde_json::json!({"agents":[{"name":"beta.hijacked","description":"forged","online":true}]}).to_string();
    assert_eq!(
        post(
            &central_process.address,
            "/directory/report",
            AHOST,
            "alpha.relay",
            &forged
        ),
        400
    );
    ar.stop();
    br.stop();
    central_process.stop();
    let next = Process::start(vec![
        "hub".into(),
        "--listen".into(),
        central.clone(),
        "--config".into(),
        temp.0.join("hub.toml").display().to_string(),
    ]);
    let token = Token::read(&temp.0.join("alpha.host")).unwrap();
    let (status, body) = http::post_response_from(
        &next.address,
        "/directory/list",
        &token,
        Some("alpha.reader"),
        "{}",
        Duration::from_secs(3),
    )
    .unwrap();
    assert_eq!(status, 200);
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(page["agents"].as_array().unwrap().len(), 2);
    assert!(
        page["agents"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["status"] == "offline" && row["host_online"] == false)
    );
    assert!(page["agents"][0]["last_seen"].is_string());
    assert!(!body.contains("hijacked"));
}

#[test]
fn directory_marks_a_host_offline_after_freshness_expires() {
    let temp = Temp::new();
    let reserved = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = reserved.local_addr().unwrap().to_string();
    let config = format!(
        "directory_ttl_secs=1\n[[hosts]]\nname='alpha'\nrelay='{endpoint}'\ntoken_file='alpha.host'\n"
    );
    let path = temp.0.join("ttl.toml");
    std::fs::write(&path, config).unwrap();
    let central = Process::start(vec![
        "hub".into(),
        "--listen".into(),
        "0".into(),
        "--config".into(),
        path.display().to_string(),
    ]);
    let report=serde_json::json!({"agents":[{"name":"alpha.alice","description":"owns tests","online":true}]}).to_string();
    assert_eq!(
        post(
            &central.address,
            "/directory/report",
            AHOST,
            "alpha.relay",
            &report
        ),
        200
    );
    let token = Token::read(&temp.0.join("alpha.host")).unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    let (_, body) = http::post_response_from(
        &central.address,
        "/directory/list",
        &token,
        Some("alpha.alice"),
        "{}",
        Duration::from_secs(3),
    )
    .unwrap();
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(page["agents"][0]["status"], "offline");
    assert_eq!(page["agents"][0]["host_online"], false);
}
