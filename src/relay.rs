use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc, Mutex, RwLock,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use crate::{Result, http, hub, journal, state, token::Token};
use serde::Deserialize;
use subtle::ConstantTimeEq;

#[path = "relay_sessions.rs"]
mod sessions;

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct Config {
    host: String,
    hub: String,
    host_token_file: PathBuf,
    journal: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    tunnel_script: Option<PathBuf>,
    tunnel_env: Option<PathBuf>,
    directory_report_secs: Option<u64>,
    control_socket: Option<PathBuf>,
    session_ttl_secs: Option<u64>,
    #[serde(default)]
    agents: Vec<AgentConfig>,
}
#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct AgentConfig {
    name: String,
    description: String,
    inbox: Option<String>,
    token_file: Option<PathBuf>,
    sender_token_file: PathBuf,
}
#[derive(Clone)]
struct Agent {
    inbox: Option<String>,
    description: String,
    token: Option<Token>,
    sender: Token,
    session: Option<sessions::Session>,
}

impl Agent {
    fn live(&self) -> bool {
        self.session
            .as_ref()
            .is_none_or(|s| s.expires > Instant::now())
    }
}

#[derive(Clone)]
struct Router {
    host: String,
    hub: String,
    host_token: Token,
    agents: HashMap<String, Agent>,
    log: PathBuf,
    generation: u64,
    session_ttl: Duration,
}
impl Router {
    fn load(path: &Path) -> Result<(Self, Config)> {
        let config: Config =
            toml::from_str(&std::fs::read_to_string(path).map_err(|_| "cannot read relay config")?)
                .map_err(|_| "invalid relay TOML; check fields and syntax")?;
        hub::validate_component(&config.host)?;
        if !matches!(config.directory_report_secs.unwrap_or(15), 1..=3600) {
            return Err("directory report interval must be 1..3600 seconds".into());
        }
        if !matches!(config.session_ttl_secs.unwrap_or(45), 1..=3600) {
            return Err("session TTL must be 1..3600 seconds".into());
        }
        let central = hub::loopback_endpoint(&config.hub)?;
        let host_token = Token::read(&hub::relative_to(path, config.host_token_file.clone()))?;
        if config.agents.len() > 256 {
            return Err("relay supports at most 256 agents".into());
        }
        let log = match &config.journal {
            Some(log) => hub::relative_to(path, log.clone()),
            None => journal::default_path()?,
        };
        let mut agents: HashMap<String, Agent> = HashMap::new();
        for agent in &config.agents {
            hub::validate_component(&agent.name)?;
            crate::directory::description(&agent.description)?;
            let (inbox, token) = match (&agent.inbox, &agent.token_file) {
                (Some(inbox), Some(token)) => (
                    Some(hub::loopback_endpoint(inbox)?),
                    Some(Token::read(&hub::relative_to(path, token.clone()))?),
                ),
                (None, None) => (None, None),
                _ => return Err("relay inbox and token_file must be supplied together".into()),
            };
            let sender = Token::read(&hub::relative_to(path, agent.sender_token_file.clone()))?;
            if agents.contains_key(&agent.name) {
                return Err("duplicate relay agent name".into());
            }
            if std::iter::once(&sender)
                .chain(token.iter())
                .any(|candidate| {
                    bool::from(candidate.bytes().ct_eq(host_token.bytes()))
                        || agents.values().any(|existing| {
                            std::iter::once(&existing.sender)
                                .chain(existing.token.iter())
                                .any(|other| bool::from(candidate.bytes().ct_eq(other.bytes())))
                        })
                })
                || token
                    .as_ref()
                    .is_some_and(|token| bool::from(token.bytes().ct_eq(sender.bytes())))
            {
                return Err(
                    "local sender tokens must be independent of the host and other senders".into(),
                );
            }
            agents.insert(
                agent.name.clone(),
                Agent {
                    inbox,
                    description: agent.description.clone(),
                    token,
                    sender,
                    session: None,
                },
            );
        }
        let worst = serde_json::json!({"agents":agents.iter().map(|(name,agent)|crate::directory::Report{name:format!("{}.{name}",config.host),description:agent.description.clone(),online:false}).collect::<Vec<_>>()});
        if worst.to_string().len() > crate::MAX_MESSAGE_BYTES {
            return Err("directory report exceeds 64 KiB; reduce agent descriptions/count".into());
        }
        Ok((
            Self {
                host: config.host.clone(),
                hub: central,
                host_token,
                agents,
                log,
                generation: 0,
                session_ttl: Duration::from_secs(config.session_ttl_secs.unwrap_or(45)),
            },
            config,
        ))
    }
    fn authenticate(&self, header: &[u8]) -> Option<http::SenderIdentity> {
        let mut identity = if self.host_token.authorizes(header) {
            Some(http::SenderIdentity::Hub)
        } else {
            None
        };
        for (name, agent) in &self.agents {
            if agent.live() && agent.sender.authorizes(header) {
                identity = Some(http::SenderIdentity::Authenticated(format!(
                    "{}.{name}",
                    self.host
                )));
            }
        }
        identity
    }
    fn forward(
        &self,
        path: &str,
        text: &str,
        sender: Option<&str>,
        origin: http::Origin,
    ) -> http::Response {
        if path == "/directory/list" || path.starts_with("/chats/") {
            if origin != http::Origin::Sender {
                return http::Response::new(
                    403,
                    "only local authenticated agents may query the directory",
                );
            }
            let response = http::post_response_from(
                &self.hub,
                path,
                &self.host_token,
                sender,
                text,
                Duration::from_secs(20),
            );
            return match response {
                Ok((code @ (200 | 400 | 403 | 404 | 409 | 500), body)) => {
                    match serde_json::from_str(&body) {
                        Ok(value) => http::Response::json(code, value),
                        Err(_) if path.starts_with("/chats/") && code != 200 => {
                            http::Response::new(
                                code,
                                "chat route request rejected; inspect current registration",
                            )
                        }
                        Err(_) => http::Response::new(502, "invalid directory response"),
                    }
                }
                _ => http::Response::new(503, "hub directory unavailable"),
            };
        }
        let Some(target) = path.strip_prefix("/to/") else {
            return http::Response::new(404, "unknown relay endpoint");
        };
        let Some(sender) = sender else {
            return http::Response::new(400, "sender required");
        };
        if hub::validate_name(target).is_err() || hub::validate_name(sender).is_err() {
            return http::Response::new(400, "invalid logical name");
        }
        let local = hub::qualified(target)
            .ok()
            .filter(|(host, _)| *host == self.host)
            .map(|(_, agent)| agent);
        if origin == http::Origin::Hub && local.is_none() {
            return http::Response::new(403, "inbound hub requests must target this host");
        }
        if origin != http::Origin::Hub && origin != http::Origin::Sender {
            return http::Response::new(403, "untrusted relay source");
        }
        if journal::append(&self.log, sender, target, text).is_err() {
            return http::Response::new(500, "relay journal failed; message not forwarded");
        }
        let outcome = if let Some(name) = local {
            let Some(agent) = self.agents.get(name).filter(|agent| agent.live()) else {
                return http::Response::new(404, "unknown local agent; message saved");
            };
            let (Some(inbox), Some(token)) = (&agent.inbox, &agent.token) else {
                return http::Response::new(405, "sender-only agent has no inbox; message saved");
            };
            http::post(
                inbox,
                "/",
                token,
                Some(sender),
                text,
                Duration::from_secs(5),
            )
        } else {
            http::post(
                &self.hub,
                path,
                &self.host_token,
                Some(sender),
                text,
                Duration::from_secs(20),
            )
        };
        match outcome {
            Ok(200) => http::Response::new(200, "destination accepted message"),
            Ok(202) => http::Response::new(202, "destination queued message"),
            Ok(_) => http::Response::new(502, "destination rejected message; relay saved it"),
            Err(_) => http::Response::new(
                503,
                "destination unavailable or unconfirmed; relay saved it; do not blindly resend",
            ),
        }
    }
}

fn reachable(address: &str) -> bool {
    let Ok(address) = address.parse() else {
        return false;
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(300)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(300)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(300)));
    if stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut response = String::new();
    stream.take(512).read_to_string(&mut response).is_ok()
        && response.starts_with("HTTP/1.1 200 ")
        && response.contains("agent-inbox ok")
}

fn reports(router: &Router) -> Vec<crate::directory::Report> {
    let entries: Vec<_> = router
        .agents
        .iter()
        .filter(|(_, agent)| agent.live())
        .filter(|(_, agent)| agent.inbox.is_some())
        .collect();
    let result = Mutex::new(Vec::new());
    let cursor = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..entries.len().min(8) {
            scope.spawn(|| {
                loop {
                    let index = cursor.fetch_add(1, Ordering::Relaxed);
                    let Some((name, agent)) = entries.get(index) else {
                        break;
                    };
                    let row = crate::directory::Report {
                        name: format!("{}.{name}", router.host),
                        description: agent.description.clone(),
                        online: reachable(agent.inbox.as_deref().unwrap()),
                    };
                    result.lock().unwrap().push(row);
                }
            });
        }
    });
    result.into_inner().unwrap()
}

fn directory_loop(path: PathBuf, initial: Config, router: Arc<RwLock<Arc<Router>>>) {
    let mut previous = std::fs::read(&path).unwrap_or_default();
    let mut last = None;
    let mut waiting_for_tunnel = false;
    let mut reported_generation = None;
    while !STOP.load(Ordering::Relaxed) {
        let bytes = std::fs::read(&path).unwrap_or_default();
        let mut changed = false;
        if bytes != previous {
            match Router::load(&path) {
                Ok((mut next, config)) => {
                    // Merge and replacement are one mutation with registration/heartbeats.
                    let mut current = router.write().unwrap();
                    let same = next.host == current.host
                        && next.hub == current.hub
                        && bool::from(next.host_token.bytes().ct_eq(current.host_token.bytes()))
                        && config.tunnel_script == initial.tunnel_script
                        && config.tunnel_env == initial.tunnel_env
                        && config.state_dir == initial.state_dir;
                    let same = same
                        && config.control_socket == initial.control_socket
                        && config.session_ttl_secs == initial.session_ttl_secs;
                    let merge = sessions::preserve(&mut next, &current);
                    if !same {
                        eprintln!(
                            "agent-inbox: relay identity/tunnel changes require a service restart"
                        );
                    } else if merge.is_err() {
                        eprintln!(
                            "agent-inbox: static relay configuration conflicts with live sessions or capacity; retaining active routing"
                        );
                    } else {
                        *current = Arc::new(next);
                        changed = true;
                        eprintln!("agent-inbox: relay agent configuration reloaded");
                    }
                }
                Err(_) => eprintln!(
                    "agent-inbox: invalid relay configuration update; retaining active routing"
                ),
            }
            previous = bytes;
        }
        if changed
            || router
                .read()
                .is_ok_and(|current| Some(current.generation) != reported_generation)
            || last.is_none_or(|when: Instant| {
                when.elapsed() >= Duration::from_secs(initial.directory_report_secs.unwrap_or(15))
            })
        {
            let current = Arc::clone(&router.read().unwrap());
            let body = serde_json::json!({"agents":reports(&current)}).to_string();
            let from = format!("{}.relay", current.host);
            match http::post_diagnostic(
                &current.hub,
                "/directory/report",
                &current.host_token,
                Some(&from),
                &body,
                Duration::from_secs(20),
            ) {
                Ok((200, _)) => {
                    if waiting_for_tunnel {
                        eprintln!(
                            "agent-inbox: directory hub connection restored at {}; report accepted",
                            current.hub
                        );
                    }
                    waiting_for_tunnel = false;
                    last = Some(Instant::now());
                    reported_generation = Some(current.generation);
                }
                Ok((status, reason)) => {
                    waiting_for_tunnel = false;
                    eprintln!(
                        "agent-inbox: directory report unconfirmed: HTTP {status}: {}; will retry",
                        http::response_reason(&reason)
                    );
                    last = None;
                }
                Err(error)
                    if error.to_string()
                        == "cannot connect to inbox; message was not confirmed" =>
                {
                    if !waiting_for_tunnel {
                        eprintln!(
                            "agent-inbox: directory waiting for tunnel: cannot connect to hub at {}",
                            current.hub
                        );
                    }
                    waiting_for_tunnel = true;
                    last = None;
                }
                Err(error) => {
                    waiting_for_tunnel = false;
                    eprintln!("agent-inbox: directory report unconfirmed: {error}; will retry");
                    last = None;
                }
            }
        }
        for _ in 0..10 {
            if STOP.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}
struct Tunnel(Arc<Mutex<Child>>);
impl Drop for Tunnel {
    fn drop(&mut self) {
        // The supervisor owns its guard/SSH groups and handles TERM before we fall back to KILL.
        let Ok(mut child) = self.0.lock() else {
            return;
        };
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        unsafe {
            libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

pub(crate) fn validate_for_daemon(path: &Path, no_tunnel: bool) -> Result<Option<PathBuf>> {
    let (_, config) = Router::load(path)?;
    if !no_tunnel {
        for file in [config.tunnel_script.as_ref(), config.tunnel_env.as_ref()] {
            if !file.is_some_and(|p| hub::relative_to(path, p.clone()).is_file()) {
                return Err(
                    "desktop relay requires readable tunnel_script/tunnel_env or no_tunnel=true"
                        .into(),
                );
            }
        }
    }
    Ok(config.control_socket.map(|p| hub::relative_to(path, p)))
}

pub fn run(mut args: impl Iterator<Item = String>) -> Result<()> {
    let mut listen = None;
    let mut path = None;
    let mut state_dir = None;
    let mut no_tunnel = false;
    let mut parent = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!(
                    "agent-inbox relay --listen <loopback-address> --config <toml> [--state-dir <path>] [--no-tunnel]"
                );
                return Ok(());
            }
            "--no-tunnel" => no_tunnel = true,
            "--parent" => parent = Some(args.next().ok_or("missing relay parent")?.parse::<u32>()?),
            "--listen" | "--config" | "--state-dir" => {
                let value = args.next().ok_or("missing relay option value")?;
                match arg.as_str() {
                    "--listen" => listen = Some(http::listen_address(&value)?),
                    "--config" => path = Some(PathBuf::from(value)),
                    _ => state_dir = Some(PathBuf::from(value)),
                }
            }
            _ => return Err("unknown relay option".into()),
        }
    }
    let listen = listen.ok_or("relay requires --listen")?;
    if !listen.ip().is_loopback() {
        return Err("relay must bind loopback".into());
    }
    let path = std::fs::canonicalize(path.ok_or("relay requires --config")?)?;
    let (router, config) = Router::load(&path)?;
    let initial = config.clone();
    let root = match state_dir.or(config.state_dir.clone()) {
        Some(pathbuf) => hub::relative_to(&path, pathbuf),
        None => journal::default_path()?
            .parent()
            .ok_or("state directory unavailable")?
            .to_owned(),
    };
    let lock_path = root.join(format!("host-{}.relay-lock", router.host));
    state::parent(&lock_path)?;
    let _instance = state::open_private(&lock_path, true)?;
    _instance
        .try_lock()
        .map_err(|_| "a relay for this host is already running")?;
    let listener = TcpListener::bind(listen)?;
    let address = listener.local_addr()?;
    for signal in [libc::SIGHUP, libc::SIGINT, libc::SIGTERM] {
        if unsafe { libc::signal(signal, stop as *const () as libc::sighandler_t) } == libc::SIG_ERR
        {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    crate::daemon::watch_parent(parent, &STOP)?;
    let _tunnel = if no_tunnel {
        None
    } else {
        let script = hub::relative_to(
            &path,
            config
                .tunnel_script
                .ok_or("relay requires tunnel_script or explicit --no-tunnel")?,
        );
        let env = hub::relative_to(&path, config.tunnel_env.ok_or("relay requires tunnel_env")?);
        Some(Tunnel(Arc::new(Mutex::new(
            Command::new("sh")
                .arg(script)
                .args(["--config"])
                .arg(env)
                .env("INBOX_SUPERVISOR_PARENT", std::process::id().to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()?,
        ))))
    };
    let running = Arc::new(AtomicBool::new(true));
    let control = Arc::clone(&running);
    let monitor = _tunnel.as_ref().map(|tunnel| Arc::clone(&tunnel.0));
    std::thread::spawn(move || {
        while !STOP.load(Ordering::Relaxed) {
            if let Some(child) = &monitor
                && child
                    .lock()
                    .ok()
                    .and_then(|mut child| child.try_wait().ok().flatten())
                    .is_some()
            {
                eprintln!("agent-inbox: tunnel supervisor exited; relay service must restart");
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        control.store(false, Ordering::Release);
    });
    let router = Arc::new(RwLock::new(Arc::new(router)));
    let socket = match initial.control_socket.clone() {
        Some(socket) => hub::relative_to(&path, socket),
        None => crate::local_control::default_path(address.port())?,
    };
    let registrations = Arc::clone(&router);
    let expiry = Arc::clone(&router);
    let _local = crate::local_control::Server::start(
        socket,
        Arc::new(move |value| sessions::apply(&registrations, value)),
        Arc::new(move || sessions::expire(&expiry)),
    )?;
    let registry = Arc::clone(&router);
    std::thread::spawn(move || directory_loop(path, initial, registry));
    let auth = Arc::clone(&router);
    let authenticate = Arc::new(move |header: &[u8]| auth.read().ok()?.authenticate(header));
    let handler = Arc::new(
        move |path: &str, text: &str, sender: Option<&str>, origin: http::Origin| {
            let snapshot = router.read().map(|router| Arc::clone(&router));
            match snapshot {
                Ok(router) => router.forward(path, text, sender, origin),
                Err(_) => http::Response::new(500, "relay routing unavailable"),
            }
        },
    );
    eprintln!("agent-inbox: listening on http://{address}/");
    http::serve_authenticated(listener, running, authenticate, handler)
}
