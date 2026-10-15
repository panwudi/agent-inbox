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
    #[serde(default)]
    agents: Vec<AgentConfig>,
}
#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct AgentConfig {
    name: String,
    description: String,
    inbox: String,
    token_file: PathBuf,
    sender_token_file: PathBuf,
}
struct Agent {
    inbox: String,
    description: String,
    token: Token,
    sender: Token,
}

struct Router {
    host: String,
    hub: String,
    host_token: Token,
    agents: HashMap<String, Agent>,
    log: PathBuf,
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
            let inbox = hub::loopback_endpoint(&agent.inbox)?;
            let token = Token::read(&hub::relative_to(path, agent.token_file.clone()))?;
            let sender = Token::read(&hub::relative_to(path, agent.sender_token_file.clone()))?;
            if agents.contains_key(&agent.name) {
                return Err("duplicate relay agent name".into());
            }
            if bool::from(sender.bytes().ct_eq(host_token.bytes()))
                || bool::from(token.bytes().ct_eq(host_token.bytes()))
                || bool::from(token.bytes().ct_eq(sender.bytes()))
                || agents.values().any(|existing| {
                    bool::from(existing.sender.bytes().ct_eq(sender.bytes()))
                        || bool::from(existing.token.bytes().ct_eq(sender.bytes()))
                        || bool::from(existing.sender.bytes().ct_eq(token.bytes()))
                        || bool::from(existing.token.bytes().ct_eq(token.bytes()))
                })
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
            if agent.sender.authorizes(header) {
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
        if path == "/directory/list" {
            if origin != http::Origin::Sender {
                return http::Response::new(
                    403,
                    "only local authenticated agents may query the directory",
                );
            }
            return match http::post_response_from(
                &self.hub,
                path,
                &self.host_token,
                sender,
                text,
                Duration::from_secs(20),
            ) {
                Ok((200, body)) => match serde_json::from_str(&body) {
                    Ok(value) => http::Response::json(200, value),
                    Err(_) => http::Response::new(502, "invalid directory response"),
                },
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
            let Some(agent) = self.agents.get(name) else {
                return http::Response::new(404, "unknown local agent; message saved");
            };
            http::post(
                &agent.inbox,
                "/",
                &agent.token,
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
    let entries: Vec<_> = router.agents.iter().collect();
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
                        online: reachable(&agent.inbox),
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
    while !STOP.load(Ordering::Relaxed) {
        let bytes = std::fs::read(&path).unwrap_or_default();
        let mut changed = false;
        if bytes != previous {
            match Router::load(&path) {
                Ok((next, config)) => {
                    let current = router.read().unwrap();
                    let same = next.host == current.host
                        && next.hub == current.hub
                        && bool::from(next.host_token.bytes().ct_eq(current.host_token.bytes()))
                        && config.tunnel_script == initial.tunnel_script
                        && config.tunnel_env == initial.tunnel_env
                        && config.state_dir == initial.state_dir;
                    drop(current);
                    if same {
                        *router.write().unwrap() = Arc::new(next);
                        changed = true;
                        eprintln!("agent-inbox: relay agent configuration reloaded");
                    } else {
                        eprintln!(
                            "agent-inbox: relay identity/tunnel changes require a service restart"
                        );
                    }
                }
                Err(_) => eprintln!(
                    "agent-inbox: invalid relay configuration update; retaining active routing"
                ),
            }
            previous = bytes;
        }
        if changed
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
                    waiting_for_tunnel = false;
                    last = Some(Instant::now());
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
                        eprintln!("agent-inbox: directory waiting for tunnel");
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

pub fn run(mut args: impl Iterator<Item = String>) -> Result<()> {
    let mut listen = None;
    let mut path = None;
    let mut state_dir = None;
    let mut no_tunnel = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!(
                    "agent-inbox relay --listen <loopback-address> --config <toml> [--state-dir <path>] [--no-tunnel]"
                );
                return Ok(());
            }
            "--no-tunnel" => no_tunnel = true,
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
    let registry = Arc::clone(&router);
    std::thread::spawn(move || directory_loop(path, initial, registry));
    let auth = Arc::clone(&router);
    let authenticate = Arc::new(move |header: &[u8]| auth.read().ok()?.authenticate(header));
    let handler = Arc::new(
        move |path: &str, text: &str, sender: Option<&str>, origin: http::Origin| match router
            .read()
        {
            Ok(router) => router.forward(path, text, sender, origin),
            Err(_) => http::Response::new(500, "relay routing unavailable"),
        },
    );
    eprintln!("agent-inbox: listening on http://{address}/");
    http::serve_authenticated(listener, running, authenticate, handler)
}
