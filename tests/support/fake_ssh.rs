use std::io::Write;
use std::os::unix::net::UnixListener;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn log(kind: &str, args: &[String]) {
    let path = std::env::var("INBOX_FAKE_SSH_LOG").unwrap();
    let millis = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
    let mut file = std::fs::OpenOptions::new().append(true).create(true).open(path).unwrap();
    let parent = std::process::Command::new("ps").args(["-o", "ppid=", "-p", &std::process::id().to_string()]).output().unwrap();
    let parent = String::from_utf8(parent.stdout).unwrap();
    writeln!(file, "{kind}\t{millis}\t{}\t{}\t{}", std::process::id(), parent.trim(), args.join("\t")).unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--warmup"] { return; }
    if args == ["facts"] {
        log("facts", &args);
        print!("{}", std::fs::read_to_string(std::env::var("INBOX_FAKE_FACTS_FILE").unwrap()).unwrap());
        return;
    }
    if !args.iter().any(|arg| arg == "-N") {
        log("transport", &args);
        print!("raw transport ready");
        return;
    }
    let path = std::env::var("INBOX_FAKE_SSH_LOG").unwrap();
    let previous = std::fs::read_to_string(path).unwrap_or_default();
    log("dial", &args);
    let behavior = std::env::var("INBOX_FAKE_SSH_BEHAVIOR").unwrap_or_else(|_| "hold".into());
    if behavior == "fail" || behavior == "fail-then-hold" && !previous.lines().any(|line| line.starts_with("dial\t")) {
        std::process::exit(255);
    }
    if behavior == "hang-auth" { std::thread::sleep(Duration::from_secs(120)); }
    if behavior == "slow-auth" { std::thread::sleep(Duration::from_secs(1)); }
    let control = args.windows(2).find(|pair| pair[0] == "-S").unwrap();
    let _socket = UnixListener::bind(&control[1]).unwrap();
    log("auth", &args);
    std::thread::sleep(Duration::from_secs(120));
}
