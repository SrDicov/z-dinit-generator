// z-dinit-generator — perfect dinit replacement frontend for zinit.
//
// One binary, std-only, zero deps: `rustc -O --edition=2021 z-dinit-generator.rs`.
// Verbs:
//   convert <dinitdir> <outdir>   live conversion, text in / text out (mode 1)
//   watch <dinitdir> <outdir>      background real-time translator (mode 2)
//   dinitctl <args...>             dinit CLI emulator over zinit C4
//   compatd [/run/dinitctl]        minimal dinit-protocol server (devmon)
//   start [--out DIR]              live switch: convert system dirs, install
//                                  compatd + watch services, print next steps
//
// Unix rules: silent on success, stderr for warnings, exit 0 ok / 1 I/O /
// 2 usage. POSIX sh semantics are mirrored bug-for-bug so `convert` output
// is byte-identical to dinit-import.sh.
// ponytail: watch polls mtimes every 2s; inotify if sub-second ever matters.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};

const VERSION: &str = "0.1.0";
const DEFAULT_SOCKET: &str = "/run/zinit/ctl";
const SOCKET_ENV: &str = "ZINIT_SOCKET";
const DINIT_SOCK_DEFAULT: &str = "/run/dinitctl";

// --- dinit protocol (compatd) ---
const QUERYVERSION: u8 = 0;
const FINDSERVICE: u8 = 1;
const LOADSERVICE: u8 = 2;
const WAKESERVICE: u8 = 5;
const CLOSEHANDLE: u8 = 23;
const CPVERSION: u8 = 58;
const SERVICERECORD: u8 = 59;
const ACK: u8 = 50;
const BADREQ: u8 = 52;
const VER_REPLY: [u8; 5] = [CPVERSION, 1, 0, 7, 0];

fn warn(msg: &str) {
    eprintln!("z-dinit-generator: {msg}");
}

fn usage() -> i32 {
    eprintln!(
        "usage: z-dinit-generator <convert|watch|dinitctl|compatd|start> [...]\n\
         convert <dinitdir> <outdir> | watch <dinitdir> <outdir> |\n\
         dinitctl <cmd> [args] | compatd [/run/dinitctl] | start [--out DIR]"
    );
    2
}

// ---------- convert (mode 1) ----------

fn trim_spaces(s: &str) -> &str {
    s.trim_matches(|c| c == ' ' || c == '\t' || c == '\r')
}

fn is_blank_spaces(s: &str) -> bool {
    s.is_empty() || s.bytes().all(|b| b == b' ')
}

fn dur(v: &str) -> String {
    if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) {
        format!("{v}s")
    } else {
        v.to_string()
    }
}

fn squote(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('\'');
    for c in s.chars() {
        if c == '\'' {
            o.push_str("'\\''");
        } else {
            o.push(c);
        }
    }
    o.push('\'');
    o
}

fn valid_name(n: &str) -> bool {
    if n.is_empty() || n.starts_with('.') || n.starts_with('@') || n.ends_with('@') {
        return false;
    }
    if n.contains("..") {
        return false;
    }
    if n.bytes().filter(|&b| b == b'@').count() > 1 {
        return false;
    }
    n.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'@' || b == b'+' || b == b'-')
}

/// Split one logical line: `+=` wins over `=`, which wins over `:`.
/// Returns (key, value, append).
fn split_line(ln: &str) -> Option<(&str, &str, bool)> {
    if let Some(i) = ln.find("+=") {
        return Some((&ln[..i], &ln[i + 2..], true));
    }
    if let Some(i) = ln.find('=') {
        return Some((&ln[..i], &ln[i + 1..], false));
    }
    if let Some(i) = ln.find(':') {
        return Some((&ln[..i], &ln[i + 1..], false));
    }
    None
}

struct Converted {
    name: String,
    text: String,
}

#[allow(clippy::too_many_arguments)]
fn convert_one(
    file_text: &str,
    n: &str,
    in_dir: &std::path::Path,
    pulls: &mut BTreeMap<String, Vec<String>>,
    befores: &mut Vec<(String, String)>,
    warns: &mut Vec<String>,
) -> Option<Converted> {
    let mut w = |m: String| warns.push(format!("{n}: {m}"));
    let mut stype = String::new();
    let mut command = String::new();
    let mut deps = String::new();
    let mut restart = String::new();
    let mut budget = String::new();
    let mut stop = String::new();
    let mut start = String::new();
    let mut user = String::new();
    let mut wdir = String::new();
    let mut log = String::new();
    let mut pid = String::new();
    let mut ready = String::new();
    let mut notifyfd = String::new();
    let mut stopcmd = String::new();
    let mut chain = String::new();
    let mut chainalways = false;

    for raw in file_text.lines() {
        // strip comment like sed 's/#.*$//'
        let no_comment = match raw.find('#') {
            Some(i) => &raw[..i],
            None => raw,
        };
        let ln = no_comment.strip_suffix('\r').unwrap_or(no_comment);
        if is_blank_spaces(ln) {
            continue;
        }
        let (k_raw, v_raw, ap) = match split_line(ln) {
            Some(t) => t,
            None => {
                w("bad line, ignored".to_string());
                continue;
            }
        };
        let k = trim_spaces(k_raw).to_string();
        let v = trim_spaces(v_raw).to_string();
        match k.as_str() {
            "type" => stype = v,
            "command" => {
                if ap {
                    command.push(' ');
                    command.push_str(&v);
                } else {
                    command = v;
                }
            }
            "depends-on" | "depends-ms" => {
                if !deps.is_empty() {
                    deps.push_str(", ");
                }
                deps.push_str(&v);
                pulls.entry(n.to_string()).or_default().push(v);
            }
            "waits-for" => {
                if !deps.is_empty() {
                    deps.push_str(", ");
                }
                deps.push_str("opt:");
                deps.push_str(&v);
                pulls.entry(n.to_string()).or_default().push(v);
            }
            "after" => {
                if !deps.is_empty() {
                    deps.push_str(", ");
                }
                deps.push_str("opt:");
                deps.push_str(&v);
            }
            "before" => befores.push((v, format!("opt:{n}"))),
            "depends-on.d" | "depends-ms.d" | "waits-for.d" => {
                let pre = if k == "depends-on.d" { "" } else { "opt:" };
                let dd = v.clone();
                let bn = dd.rsplit('/').next().unwrap_or(&dd).to_string();
                let cand_in = in_dir.join(&bn);
                let dd_path = if cand_in.is_dir() {
                    cand_in
                } else {
                    std::path::PathBuf::from(&dd)
                };
                match std::fs::read_dir(&dd_path) {
                    Ok(rd) => {
                        let mut names: Vec<String> = Vec::new();
                        for e in rd.flatten() {
                            if let Some(s) = e.file_name().to_str() {
                                names.push(s.to_string());
                            }
                        }
                        names.sort();
                        for e in names {
                            if !deps.is_empty() {
                                deps.push_str(", ");
                            }
                            deps.push_str(pre);
                            deps.push_str(&e);
                            pulls.entry(n.to_string()).or_default().push(e);
                        }
                    }
                    Err(_) => w(format!("{k} {v} missing, ignored")),
                }
            }
            "ready-notification" => {
                ready = "notify".to_string();
                if let Some(rest) = v.strip_prefix("pipefd:") {
                    if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
                        notifyfd = rest.to_string();
                    } else {
                        w(format!("ready {v} ignored"));
                    }
                } else {
                    w(format!("ready {v} ignored"));
                }
            }
            "restart" => match v.as_str() {
                "true" | "yes" | "1" => restart = "always".to_string(),
                "false" | "no" | "0" => restart = "never".to_string(),
                "on-failure" => restart = "on-failure".to_string(),
                _ => w(format!("restart={v} ignored")),
            },
            k if k == "restart-delay" || k.starts_with("restart-limit-") => {
                w(format!("{k} ignored; 3-per-10s envelope used"));
            }
            "stop-command" => {
                if ap {
                    stopcmd.push(' ');
                    stopcmd.push_str(&v);
                } else {
                    stopcmd = v;
                }
            }
            "chain-to" => chain = v,
            "options" => {
                if v.contains("always-chain") {
                    chainalways = true;
                }
                w(format!("{n}: options ignored except always-chain"));
            }
            "stop-timeout" => stop = dur(&v),
            "start-timeout" => start = dur(&v),
            "run-as" => user = v,
            "working-dir" => wdir = v,
            "logfile" => log = format!("file:{v}"),
            "pid-file" => pid = v,
            _ => w(format!("{k} ignored")),
        }
    }

    let zt: &str = match stype.as_str() {
        "process" => "process",
        "bgprocess" => {
            if pid.is_empty() {
                w("bgprocess without pid-file as process".to_string());
                "process"
            } else {
                "forking"
            }
        }
        "scripted" => "oneshot",
        "internal" | "triggered" => {
            if command.is_empty() {
                command = "true".to_string();
            }
            "oneshot"
        }
        _ => {
            w(format!("type '{stype}' unknown, skipped"));
            return None;
        }
    };
    if command.is_empty() {
        w("no command, skipped".to_string());
        return None;
    }

    // Shell assembly (same comments as dinit-import.sh).
    let mut pre = String::new();
    if !wdir.is_empty() {
        pre.push_str(&format!("cd {} && ", squote(&wdir)));
    }
    if !notifyfd.is_empty() {
        if zt == "oneshot" || zt == "forking" {
            w("pipefd needs a plain process, ignored".to_string());
            notifyfd.clear();
        } else {
            pre.push_str(&format!("exec {notifyfd}>&$ZINIT_NOTIFY_FD; "));
        }
    }
    let mut stunt = String::new();
    if !stopcmd.is_empty() {
        stunt = format!("trap {} TERM; ", squote(&stopcmd));
    }
    let mut tail = String::new();
    if !chain.is_empty() {
        if !valid_name(&chain) {
            w(format!("chain-to {chain} ignored"));
            chain.clear();
        }
    }
    if !chain.is_empty() {
        if chainalways {
            tail = format!(
                "; code=$?; command -v zctl >/dev/null 2>&1 && zctl start {chain}; exit $code"
            );
        } else {
            tail = format!(
                "; code=$?; if [ $code -eq 0 ]; then command -v zctl >/dev/null 2>&1 && zctl start {chain}; fi; exit $code"
            );
        }
    }
    let mut zt_owned = zt.to_string();
    if !pre.is_empty() || !stunt.is_empty() || !tail.is_empty() {
        if zt == "forking" || zt == "oneshot" {
            if !wdir.is_empty() {
                w("working-dir needs a shell, ignored".to_string());
            }
            if !stopcmd.is_empty() {
                w("stop-command needs a shell, ignored".to_string());
            }
            if !chain.is_empty() {
                w("chain-to needs a shell, ignored".to_string());
            }
        } else {
            zt_owned = "script".to_string();
            if !stunt.is_empty() || !tail.is_empty() {
                command = format!("{stunt}{pre}{command}{tail}");
            } else {
                command = format!("{pre}exec {command}");
            }
        }
    }
    if zt_owned == "oneshot" && restart == "on-failure" {
        restart = "never".to_string();
    }
    if restart == "always" {
        budget = "3 restarts per 10s".to_string();
    } else {
        match zt_owned.as_str() {
            "process" | "forking" | "script" => {
                if restart.is_empty() {
                    restart = "always".to_string();
                    budget = "3 restarts per 10s".to_string();
                }
            }
            _ => {}
        }
    }
    if log.is_empty() {
        log = "none".to_string();
    }
    if ready.is_empty() {
        ready = "none".to_string();
    }

    let mut o = String::new();
    o.push_str(&format!("type = {zt_owned}\n"));
    o.push_str(&format!("command = {command}\n"));
    if !deps.is_empty() {
        o.push_str(&format!("depends = {deps}\n"));
    }
    if !restart.is_empty() {
        o.push_str(&format!("restart = {restart}\n"));
    }
    if !budget.is_empty() {
        o.push_str(&format!("restart-budget = {budget}\n"));
    }
    if !stop.is_empty() {
        o.push_str(&format!("stop-timeout = {stop}\n"));
    }
    if !start.is_empty() {
        o.push_str(&format!("start-timeout = {start}\n"));
    }
    if !user.is_empty() {
        o.push_str(&format!("user = {user}\n"));
    }
    if !log.is_empty() {
        o.push_str(&format!("log = {log}\n"));
    }
    if !ready.is_empty() {
        o.push_str(&format!("ready = {ready}\n"));
    }
    if zt_owned == "forking" && !pid.is_empty() {
        o.push_str(&format!("pid-file = {pid}\n"));
    }
    Some(Converted {
        name: n.to_string(),
        text: o,
    })
}

fn cmd_convert(in_dir: &std::path::Path, out_dir: &std::path::Path) -> i32 {
    if !in_dir.is_dir() {
        warn(&format!("{}: not a directory", in_dir.display()));
        return 1;
    }
    if std::fs::create_dir_all(out_dir).is_err() {
        warn("cannot create outdir");
        return 1;
    }
    let mut names: Vec<String> = Vec::new();
    let rd = match std::fs::read_dir(in_dir) {
        Ok(r) => r,
        Err(_) => {
            warn("cannot read indir");
            return 1;
        }
    };
    for e in rd.flatten() {
        let ft = match e.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if !ft.is_file() {
            continue;
        }
        if let Some(s) = e.file_name().to_str() {
            names.push(s.to_string());
        }
    }
    names.sort();
    let mut pulls: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut befores: Vec<(String, String)> = Vec::new();
    let mut converted: Vec<Converted> = Vec::new();
    let mut warns: Vec<String> = Vec::new();
    for n in &names {
        if !valid_name(n) {
            warns.push(format!("{n}: odd name, skipped"));
            continue;
        }
        let text = match std::fs::read_to_string(in_dir.join(n)) {
            Ok(t) => t,
            Err(_) => {
                warns.push(format!("{n}: cannot read, skipped"));
                continue;
            }
        };
        match convert_one(&text, n, in_dir, &mut pulls, &mut befores, &mut warns) {
            Some(c) => converted.push(c),
            None => {}
        }
    }
    // write converted
    for c in &converted {
        let p = out_dir.join(format!("{}.conf", c.name));
        if std::fs::write(&p, &c.text).is_err() {
            warns.push(format!("cannot write {}.conf", c.name));
            for m in &warns {
                warn(m);
            }
            return 1;
        }
    }
    // before inversion
    for (b, a) in &befores {
        let p = out_dir.join(format!("{b}.conf"));
        if !p.is_file() {
            warns.push(format!("{a}: before {b} unknown, ignored"));
            continue;
        }
        let cur = match std::fs::read_to_string(&p) {
            Ok(t) => t,
            Err(_) => {
                for m in &warns {
                    warn(m);
                }
                return 1;
            }
        };
        let mut out = String::new();
        let mut hit = false;
        for l in cur.lines() {
            if l.starts_with("depends = ") {
                out.push_str(&format!("depends = {a}, {}\n", &l["depends = ".len()..]));
                hit = true;
            } else {
                out.push_str(l);
                out.push('\n');
            }
        }
        if !hit {
            out.push_str(&format!("depends = {a}\n"));
        }
        if std::fs::write(&p, out).is_err() {
            for m in &warns {
                warn(m);
            }
            return 1;
        }
    }
    // boot closure: BFS from boot over recorded pulls
    if in_dir.join("boot").is_file() {
        let mut closed: BTreeSet<String> = BTreeSet::new();
        let mut work: Vec<String> = vec!["boot".to_string()];
        let mut wi = 0;
        while wi < work.len() {
            let s = work[wi].clone();
            wi += 1;
            if !closed.insert(s.clone()) {
                continue;
            }
            if let Some(ds) = pulls.get(&s) {
                for d in ds {
                    if d.is_empty() || d.starts_with('#') {
                        continue;
                    }
                    if !closed.contains(d) && !work.contains(d) {
                        work.push(d.clone());
                    }
                }
            }
        }
        let outs = match std::fs::read_dir(out_dir) {
            Ok(r) => r,
            Err(_) => {
                for m in &warns {
                    warn(m);
                }
                return 1;
            }
        };
        for e in outs.flatten() {
            let fname = match e.file_name().to_str() {
                Some(s) => s.to_string(),
                None => continue,
            };
            if !fname.ends_with(".conf") {
                continue;
            }
            let svc = fname.trim_end_matches(".conf").to_string();
            if !closed.contains(&svc) {
                let p = out_dir.join(&fname);
                let mut cur = std::fs::read_to_string(&p).unwrap_or_default();
                if !cur.ends_with('\n') && !cur.is_empty() {
                    cur.push('\n');
                }
                cur.push_str("enabled = no\n");
                if std::fs::write(&p, cur).is_err() {
                    for m in &warns {
                        warn(m);
                    }
                    return 1;
                }
            }
        }
    } else {
        warns.push("no boot service; everything enabled".to_string());
    }
    for m in &warns {
        warn(m);
    }
    0
}

// ---------- C4 client ----------

fn zinit_socket(flag: Option<&str>) -> String {
    if let Some(p) = flag {
        return p.to_string();
    }
    std::env::var(SOCKET_ENV).unwrap_or_else(|_| DEFAULT_SOCKET.to_string())
}

fn c4_transact(sock: &str, frame: &str) -> Result<String, String> {
    let io = UnixStream::connect(sock).map_err(|e| format!("cannot dial {sock}: {e}"))?;
    let mut io = BufReader::new(io);
    io.get_mut()
        .write_all(frame.as_bytes())
        .map_err(|e| format!("cannot send: {e}"))?;
    let mut reply = String::new();
    io.read_line(&mut reply)
        .map_err(|e| format!("cannot read: {e}"))?;
    Ok(reply)
}

fn c4_reload(sock: &str) {
    let frame = "C watch reload-all\n";
    match c4_transact(sock, frame) {
        Ok(_) => {}
        Err(e) => warn(&format!("reload-all: {e}")),
    }
}

// ---------- dinitctl ----------

fn cmd_dinitctl(args: &[String]) -> i32 {
    let mut argv: Vec<String> = args.to_vec();
    // strip --use-passed-cfd and --
    let mut clean: Vec<String> = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--use-passed-cfd" => {}
            "--" => {
                clean.extend_from_slice(&argv[i + 1..]);
                break;
            }
            s if s.starts_with('-') => {
                warn(&format!("unknown flag {}", argv[i]));
                return 1;
            }
            _ => clean.push(argv[i].clone()),
        }
        i += 1;
    }
    argv = clean;
    if argv.is_empty() {
        eprintln!("usage: dinitctl <status|list|start|stop|restart|is-started|is-failed|setenv|unsetenv> [args]");
        return 1;
    }
    let cmd = argv[0].clone();
    let rest = &argv[1..];
    match cmd.as_str() {
        "setenv" | "unsetenv" => {
            warn(&format!(
                "{} {} (no activation environment under zinit; ignored)",
                cmd,
                rest.join(" ")
            ));
            0
        }
        "status" | "list" | "start" | "stop" | "restart" => {
            if rest.iter().any(|s| s.chars().any(char::is_whitespace)) {
                warn("name with whitespace cannot cross C4");
                return 1;
            }
            let sock = zinit_socket(None);
            let mut frame = format!("C dinitctl {cmd}");
            for r in rest {
                frame.push(' ');
                frame.push_str(r);
            }
            frame.push('\n');
            match c4_transact(&sock, &frame) {
                Ok(reply) => {
                    print!("{reply}");
                    if reply.starts_with("R ") {
                        0
                    } else {
                        1
                    }
                }
                Err(e) => {
                    warn(&e);
                    1
                }
            }
        }
        "is-started" => {
            if rest.len() != 1 {
                warn("usage: dinitctl is-started <svc>");
                return 1;
            }
            let sock = zinit_socket(None);
            let frame = format!("C dinitctl status {}\n", rest[0]);
            match c4_transact(&sock, &frame) {
                Ok(reply) => {
                    if reply.contains(" running ") {
                        0
                    } else {
                        1
                    }
                }
                Err(_) => 1,
            }
        }
        "is-failed" => {
            if rest.len() != 1 {
                warn("usage: dinitctl is-failed <svc>");
                return 1;
            }
            let sock = zinit_socket(None);
            let frame = format!("C dinitctl status {}\n", rest[0]);
            match c4_transact(&sock, &frame) {
                Ok(reply) => {
                    if !reply.contains(" stopped desired=up ") {
                        return 1;
                    }
                    let r = match reply.find("restarts=") {
                        Some(p) => reply[p + 9..].split_whitespace().next().unwrap_or(""),
                        None => return 1,
                    };
                    let (a, b) = match r.split_once('/') {
                        Some(t) => t,
                        None => return 1,
                    };
                    let (a, b): (u64, u64) = match (a.parse(), b.parse()) {
                        (Ok(a), Ok(b)) => (a, b),
                        _ => return 1,
                    };
                    if a >= b && b > 0 {
                        0
                    } else {
                        1
                    }
                }
                Err(_) => 1,
            }
        }
        "trigger" | "wake" => {
            warn(&format!("{cmd}: no trigger concept under zinit; use start"));
            1
        }
        _ => {
            warn(&format!("unknown command {cmd}"));
            1
        }
    }
}

// ---------- compatd ----------

fn rd_n(s: &mut UnixStream, mut buf: &mut [u8]) -> bool {
    while !buf.is_empty() {
        match s.read(buf) {
            Ok(0) => return false,
            Ok(n) => buf = &mut buf[n..],
            Err(_) => return false,
        }
    }
    true
}

fn wr_all(s: &mut UnixStream, mut buf: &[u8]) -> bool {
    while !buf.is_empty() {
        match s.write(buf) {
            Ok(0) => return false,
            Ok(n) => buf = &buf[n..],
            Err(_) => return false,
        }
    }
    true
}

fn rd_u16(s: &mut UnixStream) -> Option<u16> {
    let mut b = [0u8; 2];
    rd_n(s, &mut b).then(|| u16::from_le_bytes(b))
}

fn zinit_start_best_effort(name: &str) {
    let sock = zinit_socket(None);
    let frame = format!("C compatd start {name}\n");
    // 2s cap so a wedged supervisor cannot wedge devmon.
    let _ = (|| -> Result<(), ()> {
        let io = UnixStream::connect(&sock).map_err(|_| ())?;
        let _ = io.set_read_timeout(Some(std::time::Duration::from_secs(2)));
        let mut io = BufReader::new(io);
        io.get_mut().write_all(frame.as_bytes()).map_err(|_| ())?;
        let mut reply = String::new();
        io.read_line(&mut reply).map_err(|_| ())?;
        Ok(())
    })();
}

fn serve_one(s: &mut UnixStream, mut next: u32, names: &mut BTreeMap<u32, String>) -> u32 {
    loop {
        let mut t = [0u8; 1];
        if !rd_n(s, &mut t) {
            return next;
        }
        match t[0] {
            QUERYVERSION => {
                if !wr_all(s, &VER_REPLY) {
                    return next;
                }
            }
            FINDSERVICE | LOADSERVICE => {
                let is_find = t[0] == FINDSERVICE;
                let len = match rd_u16(s) {
                    Some(l) => l,
                    None => return next,
                };
                if len == 0 || len > 1021 {
                    let _ = wr_all(s, &[BADREQ]);
                    return next;
                }
                let mut name = vec![0u8; len as usize];
                if !rd_n(s, &mut name) {
                    return next;
                }
                let svc = String::from_utf8_lossy(&name).into_owned();
                let mut reply = [SERVICERECORD, 2, 0, 0, 0, 0, 2];
                reply[2..6].copy_from_slice(&next.to_le_bytes());
                names.insert(next, svc);
                next += 1;
                let _ = is_find;
                if !wr_all(s, &reply) {
                    return next;
                }
            }
            11 | 12 | 14 => {
                let mut rest = [0u8; 9];
                if !rd_n(s, &mut rest) || !wr_all(s, &[ACK]) {
                    return next;
                }
            }
            WAKESERVICE => {
                let mut rest = [0u8; 5];
                if !rd_n(s, &mut rest) {
                    return next;
                }
                // flags(1) + handle(4): wake the named service for real.
                let h = u32::from_le_bytes([rest[1], rest[2], rest[3], rest[4]]);
                if let Some(nm) = names.get(&h).cloned() {
                    zinit_start_best_effort(&nm);
                }
                if !wr_all(s, &[ACK]) {
                    return next;
                }
            }
            CLOSEHANDLE => {
                let mut rest = [0u8; 4];
                if !rd_n(s, &mut rest) || !wr_all(s, &[ACK]) {
                    return next;
                }
                let h = u32::from_le_bytes(rest);
                names.remove(&h);
            }
            _ => {
                let _ = wr_all(s, &[BADREQ]);
                return next;
            }
        }
    }
}

fn cmd_compatd(path: &str) -> i32 {
    let _ = std::fs::remove_file(path);
    let listener = match UnixListener::bind(path) {
        Ok(l) => l,
        Err(e) => {
            warn(&format!("cannot bind {path}: {e}"));
            return 1;
        }
    };
    let mut next: u32 = 0;
    let mut names: BTreeMap<u32, String> = BTreeMap::new();
    for conn in listener.incoming() {
        if let Ok(mut s) = conn {
            next = serve_one(&mut s, next, &mut names);
        }
    }
    0
}

// ---------- watch (mode 2) ----------

fn fingerprint(dir: &std::path::Path) -> BTreeMap<String, (u64, u64)> {
    let mut m = BTreeMap::new();
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return m,
    };
    for e in rd.flatten() {
        let name = match e.file_name().to_str() {
            Some(s) => s.to_string(),
            None => continue,
        };
        let md = match e.metadata() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        m.insert(name, (mtime, md.len()));
    }
    m
}

fn cmd_watch(in_dir: &std::path::Path, out_dir: &std::path::Path) -> i32 {
    if !in_dir.is_dir() {
        warn(&format!("{}: not a directory", in_dir.display()));
        return 1;
    }
    let sock = zinit_socket(None);
    let mut last = fingerprint(in_dir);
    // initial convert so the daemon is the source of truth from second zero.
    if cmd_convert_quiet(in_dir, out_dir) != 0 {
        return 1;
    }
    c4_reload(&sock);
    loop {
        std::thread::sleep(std::time::Duration::from_secs(2));
        let cur = fingerprint(in_dir);
        if cur != last {
            last = cur;
            if cmd_convert_quiet(in_dir, out_dir) == 0 {
                c4_reload(&sock);
            }
        }
    }
}

fn cmd_convert_quiet(in_dir: &std::path::Path, out_dir: &std::path::Path) -> i32 {
    // Same as cmd_convert but warns still go to stderr (Unix: daemon is
    // silent only when there is nothing to say).
    cmd_convert(in_dir, out_dir)
}

// ---------- start (live switch) ----------

fn cmd_start(out: Option<&str>) -> i32 {
    let outdir = out.unwrap_or("/etc/zinit/services.d").to_string();
    let outp = std::path::PathBuf::from(&outdir);
    let mut found: Vec<std::path::PathBuf> = Vec::new();
    for cand in ["/usr/lib/dinit.d", "/etc/dinit.d"] {
        let p = std::path::PathBuf::from(cand);
        if p.is_dir() {
            found.push(p);
        }
    }
    if found.is_empty() {
        warn("no dinit dirs (/usr/lib/dinit.d, /etc/dinit.d)");
        return 1;
    }
    // usr first so etc wins on overlay.
    let mut n = 0;
    for d in &found {
        let before = match std::fs::read_dir(&outp) {
            Ok(r) => r.count(),
            Err(_) => 0,
        };
        let rc = cmd_convert(d, &outp);
        if rc != 0 {
            return 1;
        }
        let after = match std::fs::read_dir(&outp) {
            Ok(r) => r.count(),
            Err(_) => 0,
        };
        n += after.saturating_sub(before);
    }
    // compatd service so devmon-speaking helpers keep working.
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "z-dinit-generator".to_string());
    let compat = format!(
        "type = process\ncommand = {exe} compatd\nrestart = always\nrestart-budget = 3 restarts per 10s\nlog = none\nready = none\n"
    );
    let _ = std::fs::create_dir_all(&outp);
    if std::fs::write(outp.join("compatd.conf"), compat).is_err() {
        warn("cannot write compatd.conf");
        return 1;
    }
    // watch service so dinit files stay the source of truth (mode 2).
    let primary = found
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let watch = format!(
        "type = process\ncommand = {exe} watch {primary} {outdir}\nrestart = always\nrestart-budget = 3 restarts per 10s\nlog = none\nready = none\n"
    );
    let _ = primary;
    if std::fs::write(outp.join("dinit-watch.conf"), watch).is_err() {
        warn("cannot write dinit-watch.conf");
        return 1;
    }
    println!("converted {} services -> {}", n.max(converted_count(&outp)), outdir);
    println!("next: remove dinit, reboot on zinit");
    0
}

fn converted_count(outp: &std::path::Path) -> usize {
    match std::fs::read_dir(outp) {
        Ok(r) => r.filter(|e| e.as_ref().map(|x| x.file_name().to_str().unwrap_or("").ends_with(".conf")).unwrap_or(false)).count(),
        Err(_) => 0,
    }
}

fn main() -> std::process::ExitCode {
    // busybox style: argv[0] == dinitctl behaves as dinitctl.
    let argv0 = std::env::args().next().unwrap_or_default();
    let base = argv0.rsplit('/').next().unwrap_or(&argv0).to_string();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if base == "dinitctl" {
        return std::process::ExitCode::from(cmd_dinitctl(&args) as u8);
    }
    if args.is_empty() {
        return std::process::ExitCode::from(usage() as u8);
    }
    let verb = args.remove(0);
    let rc = match verb.as_str() {
        "--help" | "-h" | "help" => usage(),
        "--version" | "version" => {
            println!("z-dinit-generator {VERSION}");
            0
        }
        "convert" => {
            if args.len() != 2 {
                eprintln!("usage: z-dinit-generator convert <dinitdir> <outdir>");
                2
            } else {
                cmd_convert(std::path::Path::new(&args[0]), std::path::Path::new(&args[1]))
            }
        }
        "watch" => {
            if args.len() != 2 {
                eprintln!("usage: z-dinit-generator watch <dinitdir> <outdir>");
                2
            } else {
                cmd_watch(std::path::Path::new(&args[0]), std::path::Path::new(&args[1]))
            }
        }
        "dinitctl" => cmd_dinitctl(&args),
        "compatd" => {
            if args.len() > 1 {
                eprintln!("usage: z-dinit-generator compatd [/run/dinitctl]");
                2
            } else {
                cmd_compatd(args.first().map(|s| s.as_str()).unwrap_or(DINIT_SOCK_DEFAULT))
            }
        }
        "start" => {
            if args.is_empty() {
                cmd_start(None)
            } else if args.len() == 2 && args[0] == "--out" {
                cmd_start(Some(&args[1]))
            } else {
                eprintln!("usage: z-dinit-generator start [--out DIR]");
                2
            }
        }
        _ => usage(),
    };
    std::process::ExitCode::from(rc as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_is_space_oriented() {
        assert_eq!(trim_spaces("  a  "), "a");
        assert_eq!(trim_spaces("\ta\t"), "a");
        assert!(is_blank_spaces("   "));
        assert!(!is_blank_spaces(" x "));
    }

    #[test]
    fn dur_appends_s_to_bare_numbers() {
        assert_eq!(dur("5"), "5s");
        assert_eq!(dur("5s"), "5s");
        assert_eq!(dur("1m"), "1m");
    }

    #[test]
    fn names_reject_the_odd() {
        assert!(valid_name("sshd"));
        assert!(valid_name("a-early"));
        assert!(!valid_name(".hidden"));
        assert!(!valid_name("@x"));
        assert!(!valid_name("x@"));
        assert!(!valid_name("a..b"));
        assert!(!valid_name("a@b@c"));
        assert!(!valid_name("a b"));
    }

    #[test]
    fn split_prefers_plus_equals() {
        assert_eq!(split_line("a+=b"), Some(("a", "b", true)));
        assert_eq!(split_line("a=b"), Some(("a", "b", false)));
        assert_eq!(split_line("a:b"), Some(("a", "b", false)));
        assert_eq!(split_line("nope"), None);
    }

    #[test]
    fn convert_minimal_process() {
        let dir = std::env::temp_dir().join(format!("zdg-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let mut pulls = BTreeMap::new();
        let mut befores = Vec::new();
        let mut warns = Vec::new();
        let c = convert_one(
            "type = process\ncommand = /bin/sleep 300\n",
            "s",
            &dir,
            &mut pulls,
            &mut befores,
            &mut warns,
        )
        .expect("converts");
        assert!(c.text.contains("type = process\n"));
        assert!(c.text.contains("restart = always\n"));
        assert!(c.text.contains("restart-budget = 3 restarts per 10s\n"));
        assert!(c.text.contains("log = none\n"));
        assert!(c.text.contains("ready = none\n"));
    }

    // compatd framing (ported, kept green).
    fn pair() -> (UnixStream, UnixStream) {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "zdg-compatd-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("ctl");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let client = UnixStream::connect(&path).unwrap();
        let (server, _) = listener.accept().unwrap();
        let _ = client.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        (server, client)
    }

    fn read_exact(client: &mut UnixStream, n: usize) -> Vec<u8> {
        let mut buf = vec![0u8; n];
        let mut got = 0;
        while got < n {
            match client.read(&mut buf[got..]) {
                Ok(0) => break,
                Ok(k) => got += k,
                Err(e) => panic!("read: {e}"),
            }
        }
        buf.truncate(got);
        buf
    }

    #[test]
    fn version_handshake() {
        let (mut server, mut client) = pair();
        let mut names = BTreeMap::new();
        let h = std::thread::spawn(move || serve_one(&mut server, 0, &mut names));
        client.write_all(&[QUERYVERSION]).unwrap();
        assert_eq!(read_exact(&mut client, 5), VER_REPLY);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn find_returns_started_with_fresh_handles() {
        let (mut server, mut client) = pair();
        let mut names = BTreeMap::new();
        let h = std::thread::spawn(move || serve_one(&mut server, 41, &mut names));
        client.write_all(&[FINDSERVICE, 6, 0]).unwrap();
        client.write_all(b"system").unwrap();
        let r = read_exact(&mut client, 7);
        assert_eq!(&r[..2], &[SERVICERECORD, 2]);
        assert_eq!(&r[2..6], &41u32.to_le_bytes());
        assert_eq!(r[6], 2);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn wake_acks_without_zinit() {
        let (mut server, mut client) = pair();
        let mut names = BTreeMap::new();
        names.insert(7, "x".to_string());
        let h = std::thread::spawn(move || serve_one(&mut server, 8, &mut names));
        client.write_all(&[WAKESERVICE, 0, 7, 0, 0, 0]).unwrap();
        assert_eq!(read_exact(&mut client, 1), [ACK]);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn unknown_type_gets_badreq_and_close() {
        let (mut server, mut client) = pair();
        let mut names = BTreeMap::new();
        let h = std::thread::spawn(move || serve_one(&mut server, 0, &mut names));
        client.write_all(&[200]).unwrap();
        assert_eq!(read_exact(&mut client, 1), [BADREQ]);
        assert_eq!(read_exact(&mut client, 1), []);
        drop(client);
        h.join().unwrap();
    }
}
