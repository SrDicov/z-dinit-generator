// z-dinit-generator — perfect dinit replacement frontend for zinit.
//
// One binary, std-only, zero deps: `rustc -O --edition=2021 z-dinit-generator.rs`.
// Verbs:
//   convert <dinitdir> <outdir>   live conversion, text in / text out (mode 1)
//   watch <dinitdir> <outdir>      background real-time translator (mode 2)
//   dinitctl <args...>             dinit CLI emulator over zinit C4
//   compatd [/run/dinitctl] [--out DIR] full dinit-protocol server (devmon/dinitctl)
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

// --- dinit protocol (compatd): numbers mirror control-cmds.h, dinit 0.20 / v7 ---
const QUERYVERSION: u8 = 0;
const FINDSERVICE: u8 = 1;
const LOADSERVICE: u8 = 2;
const STARTSERVICE: u8 = 3;
const STOPSERVICE: u8 = 4;
const WAKESERVICE: u8 = 5;
const RELEASESERVICE: u8 = 6;
const UNPINSERVICE: u8 = 7;
const LISTSERVICES: u8 = 8;
const UNLOADSERVICE: u8 = 9;
const SHUTDOWN: u8 = 10;
const ADD_DEP: u8 = 11;
const REM_DEP: u8 = 12;
const QUERY_LOAD_MECH: u8 = 13;
const ENABLESERVICE: u8 = 14;
const QUERYSERVICENAME: u8 = 15;
const RELOADSERVICE: u8 = 16;
const SETENV: u8 = 17;
const SERVICESTATUSQ: u8 = 18;
const SETTRIGGER: u8 = 19;
const CATLOG: u8 = 20;
const SIGNALQ: u8 = 21;
const QUERYSERVICEDSCDIR: u8 = 22;
const CLOSEHANDLE: u8 = 23;
const GETALLENV: u8 = 24;
const LISTSERVICES5: u8 = 25;
const SERVICESTATUS5Q: u8 = 26;
const LISTENENV: u8 = 27;
const SERVICESTATUS6Q: u8 = 28;
const ENABLE_SERVICE_V7: u8 = 29;
const REM_DEP_V7: u8 = 30;
const ACK: u8 = 50;
const NAK: u8 = 51;
const BADREQ: u8 = 52;
const CPVERSION: u8 = 58;
const SERVICERECORD: u8 = 59;
const NOSERVICE: u8 = 60;
const ALREADYSS: u8 = 61;
const SVCINFO: u8 = 62;
const LISTDONE: u8 = 63;
const LOADER_MECH: u8 = 64;
const SERVICENAME: u8 = 66;
const SERVICESTATUSR: u8 = 70;
const SERVICE_LOG: u8 = 73;
const SIGNAL_NOPID: u8 = 74;
const SIGNAL_BADSIG: u8 = 75;
const SIGNAL_KILLERR: u8 = 76;
const SVCDSCDIR: u8 = 77;
const ALLENV: u8 = 78;
const PREACK: u8 = 79;
const SERVICEEVENT: u8 = 100;
const SERVICEEVENT5: u8 = 101;
const VER_REPLY: [u8; 5] = [CPVERSION, 1, 0, 7, 0];
// dinit service states (service-constants.h): STOPPED 0, STARTING 1, STARTED 2, STOPPING 3.
const ST_STOPPED: u8 = 0;
const ST_STARTED: u8 = 2;
// dinit stop reasons: NORMAL 0 .. TERMINATED 6; FAILED 3 marks an exhausted loop.
// dinit dependency types: REGULAR 0, WAITS_FOR 2, MILESTONE 3 (the only three
// ADD_DEP accepts; BEFORE 4 / AFTER 5 never cross the wire).

fn warn(msg: impl AsRef<str>) {
    eprintln!("z-dinit-generator: {}", msg.as_ref());
}

fn usage() -> i32 {
    eprintln!(
        "usage: z-dinit-generator <convert|watch|dinitctl|compatd|start|poweroff|reboot|halt> [...]\n\
         convert <dinitdir> <outdir> | watch <dinitdir>... <outdir> |\n\
         dinitctl <cmd> [args] | compatd [/run/dinitctl] [--out DIR] | start [--out DIR] | poweroff | reboot | halt"
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

/// Strip a dbus-style `--print-address[=FD]` token (plus one adjacent
/// space) from a command. Only whole tokens starting at a boundary are
/// removed, so `--no-print-address` and friends survive.
fn strip_print_address(cmd: &str) -> String {
    let mut o = String::with_capacity(cmd.len());
    let mut rest = cmd;
    while let Some(pos) = rest.find("--print-address") {
        let before = &rest[..pos];
        let ok_start = before.is_empty() || before.ends_with(|c| c == ' ' || c == '\t');
        let after = &rest[pos + "--print-address".len()..];
        let after = match after.strip_prefix('=') {
            Some(t) => {
                let end = t.find(|c| c == ' ' || c == '\t').unwrap_or(t.len());
                &t[end..]
            }
            None => after,
        };
        if !ok_start || !(after.is_empty() || after.starts_with(|c| c == ' ' || c == '\t')) {
            // Not a standalone token; keep it and move on.
            o.push_str(&rest[..pos + "--print-address".len()]);
            rest = after;
            continue;
        }
        o.push_str(before.trim_end_matches(|c| c == ' ' || c == '\t'));
        rest = after.trim_start_matches(|c| c == ' ' || c == '\t');
        if !rest.is_empty() {
            o.push(' ');
        }
    }
    o.push_str(rest);
    o
}

/// Strip a bare `--dinit` token (plus one adjacent space) from a command.
/// dinit's dbus patch uses it for socket activation from PID 1; under
/// supervision nobody passes the socket, so the daemon waits on an
/// activation that never comes (verified live: no bus socket, dinit-dbus
/// hangs). Only whole tokens go, so longer flags survive.
fn strip_dinit_flag(cmd: &str) -> String {
    let mut o = String::with_capacity(cmd.len());
    let mut rest = cmd;
    while let Some(pos) = rest.find("--dinit") {
        let before = &rest[..pos];
        let ok_start = before.is_empty() || before.ends_with(|c| c == ' ' || c == '\t');
        let after = &rest[pos + "--dinit".len()..];
        if !ok_start || !(after.is_empty() || after.starts_with(|c| c == ' ' || c == '\t')) {
            o.push_str(&rest[..pos + "--dinit".len()]);
            rest = after;
            continue;
        }
        o.push_str(before.trim_end_matches(|c| c == ' ' || c == '\t'));
        rest = after.trim_start_matches(|c| c == ' ' || c == '\t');
        if !o.is_empty() && !rest.is_empty() {
            o.push(' ');
        }
    }
    o.push_str(rest);
    o
}

/// dinit value cooking (load-service.h, not the manpage): only `"..."`
/// groups (single quotes are literal characters); `\\x` escapes;
/// unquoted whitespace runs collapse to one space.
fn cook_value(raw: &str) -> String {
    let mut o = String::new();
    let mut it = raw.chars().peekable();
    let mut space = true;
    let mut in_q = false;
    while let Some(c) = it.next() {
        if in_q {
            if c == '"' {
                in_q = false;
            } else if c == '\\' {
                if let Some(e) = it.next() {
                    o.push(e);
                    space = false;
                }
            } else {
                o.push(c);
                space = false;
            }
        } else if c == '"' {
            in_q = true;
        } else if c == '\'' {
            // literal upstream: kept, and the spaces around it still split.
            o.push(c);
            space = false;
        } else if c == '\\' {
            if let Some(e) = it.next() {
                o.push(e);
                space = false;
            }
        } else if c == ' ' || c == '\t' || c == '\r' {
            if !space {
                o.push(' ');
                space = true;
            }
        } else {
            o.push(c);
            space = false;
        }
    }
    if o.ends_with(' ') {
        o.pop();
    }
    o
}

/// A `#` starts a comment at line start or after whitespace (double
/// quotes honored; single quotes are literal, like upstream). A `#`
/// glued to text is INVALID dinit (load error); it stays literal here
/// and the caller warns loudly about the divergence.
fn strip_comment(ln: &str) -> &str {
    let b = ln.as_bytes();
    let mut in_q = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if in_q {
            if c == b'"' {
                in_q = false;
            } else if c == b'\\' {
                i += 1;
            }
        } else if c == b'"' {
            in_q = true;
        } else if c == b'#' && (i == 0 || b[i - 1] == b' ' || b[i - 1] == b'\t') {
            return &ln[..i];
        }
        i += 1;
    }
    ln
}

/// A `#` glued to text (outside double quotes): real dinit rejects the
/// whole file; we keep it literal and warn instead of dropping a service.
fn has_bare_hash(ln: &str) -> bool {
    let b = ln.as_bytes();
    let mut in_q = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if in_q {
            if c == b'"' {
                in_q = false;
            } else if c == b'\\' {
                i += 1;
            }
        } else if c == b'"' {
            in_q = true;
        } else if c == b'#' && !(i == 0 || b[i - 1] == b' ' || b[i - 1] == b'\t') {
            return true;
        }
        i += 1;
    }
    false
}

/// Join `\\`-continued lines with one space (odd run = continuation).
/// Returns each logical line with whether it used a join: dinit has no
/// continuations, so a file relying on one is already broken there and
/// the caller warns loudly about the divergence.
fn logical_lines(text: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut joined = false;
    for raw in text.lines() {
        let ln = raw.strip_suffix('\r').unwrap_or(raw);
        let t = ln.trim_end();
        let bs = t.len() - t.trim_end_matches('\\').len();
        if bs % 2 == 1 {
            cur.push_str(&t[..t.len() - 1]);
            cur.push(' ');
            joined = true;
        } else {
            cur.push_str(ln);
            out.push((std::mem::take(&mut cur), std::mem::take(&mut joined)));
        }
    }
    if !cur.trim().is_empty() {
        out.push((cur, joined));
    }
    out
}

/// Inline `@include`/`@include-opt` (8 deep), report the rest.
fn gather_lines(
    text: &str,
    in_dir: &std::path::Path,
    depth: u8,
    n: &str,
    warns: &mut Vec<String>,
) -> Vec<String> {
    let mut out = Vec::new();
    for (ln, joined) in logical_lines(text) {
        if joined {
            warns.push(format!("{n}: line continuation joined; dinit has none"));
        }
        let t = trim_spaces(&ln);
        if t.is_empty() {
            continue;
        }
        if t.starts_with('@') {
            let mut it = t[1..].splitn(2, char::is_whitespace);
            let cmd = it.next().unwrap_or("");
            let arg = it.next().map(trim_spaces).unwrap_or("");
            match cmd {
                "include" | "include-opt" => {
                    if depth >= 8 {
                        warns.push(format!("{n}: @include too deep, ignored"));
                        continue;
                    }
                    let q = std::path::Path::new(arg);
                    let q = if q.is_absolute() { q.to_path_buf() } else { in_dir.join(arg) };
                    match std::fs::read_to_string(&q) {
                        Ok(t2) => out.extend(gather_lines(&t2, in_dir, depth + 1, n, warns)),
                        Err(_) => {
                            if cmd == "include" {
                                warns.push(format!("{n}: @include {arg} missing"));
                            }
                        }
                    }
                }
                "meta" => warns.push(format!("{n}: @meta ignored")),
                _ => warns.push(format!("{n}: @{cmd} ignored")),
            }
            continue;
        }
        let nc = strip_comment(&ln);
        if is_blank_spaces(nc) {
            continue;
        }
        out.push(nc.to_string());
    }
    out
}

/// dinit `$1` is the instance argument; zinit spells it `%i`.
fn sub_arg1(v: &str, n: &str, warns: &mut Vec<String>) -> String {
    let tmp = v.replace("$$", "\u{0}");
    let mut o = String::new();
    let mut i = 0;
    while i < tmp.len() {
        let rest = &tmp[i..];
        if rest.starts_with("$1") {
            o.push_str("%i");
            i += 2;
        } else if rest.starts_with("${") {
            match rest.find('}') {
                Some(e) => {
                    let inner = &rest[2..e];
                    if inner == "1" {
                        o.push_str("%i");
                    } else if inner.starts_with('1') {
                        warns.push(format!("{n}: ${{{inner}}} truncated to %i"));
                        o.push_str("%i");
                    } else {
                        o.push_str(&rest[..e + 1]);
                    }
                    i += e + 1;
                }
                None => {
                    o.push('$');
                    i += 1;
                }
            }
        } else {
            let ch = rest.chars().next().unwrap();
            o.push(ch);
            i += ch.len_utf8();
        }
    }
    o.replace("\u{0}", "$$")
}

/// dinit seconds (`XXX.YYY`) into zinit duration spelling. Whole
/// seconds stay `Ns`; fractions become `Nms` (zinit rejects `1.5s`).
fn dinit_secs(v: &str) -> Option<String> {
    if v.is_empty() {
        return None;
    }
    let (int, frac) = match v.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (v, None),
    };
    if int.is_empty() || !int.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match frac {
        None => Some(format!("{int}s")),
        Some(f) => {
            if f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let mut ms: u64 = int.parse().unwrap_or(0u64).saturating_mul(1000);
            let mut scale = 100u64;
            for c in f.chars().take(3) {
                ms = ms.saturating_add((c as u64 - '0' as u64) * scale);
                scale /= 10;
            }
            Some(format!("{ms}ms"))
        }
    }
}

/// dinit `soft:hard` limit into the soft value zinit keeps.
fn soft_limit(v: &str) -> Option<String> {
    let t = v.split(':').next().unwrap_or("").trim();
    if t.is_empty() || t == "-" || !t.bytes().all(|b| b.is_ascii_digit()) {
        None
    } else {
        Some(t.to_string())
    }
}

fn valid_env_name(s: &str) -> bool {
    let mut it = s.bytes();
    match it.next() {
        Some(b) if b.is_ascii_alphabetic() || b == b'_' => {}
        _ => return false,
    }
    it.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn split_env_line(ln: &str) -> Option<(String, String)> {
    let t = ln.trim();
    if t.is_empty() || t.starts_with('#') {
        return None;
    }
    let i = t.find('=')?;
    let (k, v) = (t[..i].trim(), t[i + 1..].trim());
    if !valid_env_name(k) {
        return None;
    }
    Some((k.to_string(), v.to_string()))
}

/// /etc/passwd lookup by name or numeric id: (name, uid, gid, home, shell).
fn passwd_entry(user: &str) -> Option<(String, String, String, String, String)> {
    let text = std::fs::read_to_string("/etc/passwd").ok()?;
    for ln in text.lines() {
        let f: Vec<&str> = ln.split(':').collect();
        if f.len() < 7 {
            continue;
        }
        if f[0] == user || f[2] == user {
            return Some((
                f[0].to_string(),
                f[2].to_string(),
                f[3].to_string(),
                f[5].to_string(),
                f[6].to_string(),
            ));
        }
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
    macro_rules! w {
        ($m:expr) => {
            warns.push(format!("{n}: {}", $m))
        };
    }
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
    let mut command_raw = String::new();
    let mut pass_cs_fd = false;
    let mut stopcmd_raw = String::new();
    let mut chain = String::new();
    let mut chainalways = false;
    let mut notifyvar = String::new();
    let mut rdelay = String::new();
    let mut rcount = String::new();
    let mut rinterval = String::new();
    let mut run_console = false;
    let mut export_passwd = false;
    let mut export_svcname = false;
    let mut extra_env: Vec<String> = Vec::new();
    let mut extra_net: Vec<String> = Vec::new();
    let mut extra_rl: Vec<String> = Vec::new();
    let mut extra_cg = String::new();
    let mut extra_tty = String::new();

    for ln in gather_lines(file_text, in_dir, 0, n, warns) {
        if has_bare_hash(&ln) {
            warns.push(format!("{n}: hashmark without whitespace is invalid dinit; kept literal"));
        }
        let (k_raw, v_raw, ap) = match split_line(&ln) {
            Some(t) => t,
            None => {
                w!("bad line, ignored".to_string());
                continue;
            }
        };
        let k = trim_spaces(k_raw).to_string();
        let v = cook_value(trim_spaces(v_raw));
        match k.as_str() {
            "type" => stype = v,
            "command" => {
                let vv = sub_arg1(&v, n, warns);
                // Raw twin (quotes intact): zinit splits process commands
                // on whitespace with no quoting, so a quoted command can
                // only survive as `script`, where the shell re-parses it.
                let vr = sub_arg1(trim_spaces(v_raw), n, warns);
                if ap {
                    command.push(' ');
                    command.push_str(&vv);
                    command_raw.push(' ');
                    command_raw.push_str(&vr);
                } else {
                    command = vv;
                    command_raw = vr;
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
                deps.push_str("after:");
                deps.push_str(&v);
                pulls.entry(n.to_string()).or_default().push(v);
            }
            "after" => {
                if !deps.is_empty() {
                    deps.push_str(", ");
                }
                deps.push_str("after:");
                deps.push_str(&v);
            }
            "before" => befores.push((v, format!("after:{n}"))),
            "depends-on.d" | "depends-ms.d" | "waits-for.d" => {
                let pre = if k == "depends-on.d" { "" } else { "after:" };
                let dd = v.clone();
                let bn = dd.rsplit('/').next().unwrap_or(&dd).to_string();
                let cand_in = in_dir.join(&bn);
                // An absolute dir value names its own directory: reading
                // in_dir.join(basename) instead hijacks it whenever the
                // file's own dir happens to hold a same-named subdir
                // (Chimera: boot's `waits-for.d: /etc/dinit.d/boot.d` read
                // /usr/lib/dinit.d/boot.d, so sshd/networkmanager/tailscaled
                // fell out of the boot closure and got `enabled = no`).
                // Relative values keep the old in_dir resolution.
                let dd_path = if std::path::Path::new(&dd).is_absolute() {
                    std::path::PathBuf::from(&dd)
                } else if cand_in.is_dir() {
                    cand_in
                } else {
                    std::path::PathBuf::from(&dd)
                };
                match std::fs::read_dir(&dd_path) {
                    Ok(rd) => {
                        let mut names: Vec<String> = Vec::new();
                        for e in rd.flatten() {
                            if let Some(s) = e.file_name().to_str() {
                                // Placeholders (.KEEP) and instances are not
                                // services: the main loop skips them the same
                                // way, and an opt:.KEEP edge is a fatal E010
                                // downstream for nothing.
                                if !valid_name(s) {
                                    warns.push(format!("{n}: odd name {s} in {k}, skipped"));
                                    continue;
                                }
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
                    Err(_) => w!(format!("{k} {v} missing, ignored")),
                }
            }
            "ready-notification" => {
                ready = "notify".to_string();
                if let Some(rest) = v.strip_prefix("pipefd:") {
                    if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
                        notifyfd = rest.to_string();
                    } else {
                        w!(format!("ready {v} ignored"));
                    }
                } else if let Some(var) = v.strip_prefix("pipevar:") {
                    if valid_env_name(var) {
                        notifyvar = var.to_string();
                    } else {
                        w!(format!("ready {v} ignored"));
                    }
                } else {
                    w!(format!("ready {v} ignored"));
                }
            }
            "restart" => match v.as_str() {
                "true" | "yes" | "1" => restart = "always".to_string(),
                "false" | "no" | "0" => restart = "never".to_string(),
                "on-failure" => restart = "on-failure".to_string(),
                _ => w!(format!("restart={v} ignored")),
            },
            "restart-delay" => match dinit_secs(&v) {
                Some(d) => rdelay = d,
                None => w!(format!("restart-delay {v} ignored")),
            },
            "restart-limit-interval" => match dinit_secs(&v) {
                Some(d) => rinterval = d,
                None => w!(format!("restart-limit-interval {v} ignored")),
            },
            "restart-limit-count" => {
                if v == "0" {
                    rcount = "0".to_string();
                } else if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) {
                    rcount = v;
                } else {
                    w!(format!("restart-limit-count {v} ignored"));
                }
            }
            "stop-command" => {
                let vr = sub_arg1(trim_spaces(v_raw), n, warns);
                if ap {
                    stopcmd.push(' ');
                    stopcmd.push_str(&vr);
                    stopcmd_raw.push(' ');
                    stopcmd_raw.push_str(&vr);
                } else {
                    stopcmd = vr.clone();
                    stopcmd_raw = vr;
                }
            }
            "chain-to" => chain = v,
            "options" => {
                for o in v.split_whitespace() {
                    match o {
                        "always-chain" => chainalways = true,
                        "runs-on-console" => run_console = true,
                        // inherent: the child path always sets no-new-privs.
                        "no-new-privs" => {}
                        // dinit passes its control-socket fd; under
                        // supervision the bridge connects by path.
                        "pass-cs-fd" => pass_cs_fd = true,
                        _ => w!(format!("option {o} ignored")),
                    }
                }
            }
            "stop-timeout" => match dinit_secs(&v) {
                Some(d) => stop = d,
                None => w!("stop-timeout {v} ignored"),
            },
            "start-timeout" => match dinit_secs(&v) {
                Some(d) => start = d,
                None => w!("start-timeout {v} ignored"),
            },
            "run-as" => user = v,
            "working-dir" => wdir = sub_arg1(&v, n, warns),
            "logfile" => log = format!("file:{}", sub_arg1(&v, n, warns)),
            "pid-file" => pid = sub_arg1(&v, n, warns),
            "env-file" => {
                let q = std::path::Path::new(&v);
                let q = if q.is_absolute() { q.to_path_buf() } else { in_dir.join(&v) };
                match std::fs::read_to_string(&q) {
                    Ok(t) => {
                        for el in t.lines() {
                            match split_env_line(el) {
                                Some((k2, v2)) => extra_env.push(format!("env = {k2}={v2}")),
                                None => {
                                    let e = el.trim();
                                    if !e.is_empty() && !e.starts_with('#') {
                                        w!(format!("env-file line ignored: {e}"));
                                    }
                                }
                            }
                        }
                    }
                    Err(_) => w!(format!("env-file {v} missing, ignored")),
                }
            }
            "socket-listen" => {
                if v.is_empty() || v.chars().any(char::is_whitespace) {
                    w!(format!("socket-listen {v} ignored"));
                } else {
                    extra_net.push(format!("listen = unix:{}", sub_arg1(&v, n, warns)));
                }
            }
            "socket-permissions" | "socket-uid" | "socket-gid" => {
                w!(format!("{k} ignored; sockets bind 0600 as the service user"));
            }
            "term-signal" => {
                if v != "TERM" {
                    w!(format!("term-signal {v} ignored; TERM then KILL after stop-timeout"));
                }
            }
            "log-type" => match v.as_str() {
                "file" => {
                    if log.is_empty() {
                        w!("log-type file without logfile, ignored".to_string());
                    }
                }
                "none" => {}
                _ => w!(format!("log-type {v} has no counterpart; kept as none")),
            },
            "log-buffer-size" | "logfile-uid" | "logfile-gid" | "logfile-permissions"
            | "consumer-of" => {
                w!(format!("{k} ignored; file logs only"));
            }
            "load-options" => {
                for o in v.split_whitespace() {
                    match o {
                        "export-passwd-vars" => export_passwd = true,
                        "export-service-name" => export_svcname = true,
                        _ => w!(format!("load-option {o} ignored")),
                    }
                }
            }
            "smooth-recovery" => {
                w!("smooth-recovery ignored; restarts never stop dependents here".to_string());
            }
            "rlimit-nofile" | "rlimit-nproc" | "rlimit-as" => {
                let zn = match k.as_str() {
                    "rlimit-nofile" => "rlimit-nofile",
                    "rlimit-nproc" => "rlimit-nproc",
                    _ => "rlimit-as",
                };
                match soft_limit(&v) {
                    Some(x) => extra_rl.push(format!("{zn} = {x}")),
                    None => w!(format!("{k} {v} ignored")),
                }
            }
            "rlimit-core" | "rlimit-data" | "rlimit-addrspace" => {
                w!(format!("{k} ignored; only nofile/nproc/as exist"));
            }
            "nice" => w!(format!("nice {v} ignored; no priority directive")),
            "run-in-cgroup" => {
                if v.is_empty() {
                    w!("run-in-cgroup empty, ignored".to_string());
                } else {
                    extra_cg = format!("cgroup = {v}");
                }
            }
            "capabilities" | "securebits" => {
                w!(format!("{k} ignored; only dropping is supported, never granting"));
            }
            "ioprio" | "oom-score-adj" => w!(format!("{k} ignored")),
            "inittab-id" | "inittab-line" => w!(format!("{k} ignored; no utmp accounting")),
            _ => w!(format!("{k} ignored")),
        }
    }

    let zt: &str = match stype.as_str() {
        "process" => "process",
        "bgprocess" => {
            if pid.is_empty() {
                w!("bgprocess without pid-file as process".to_string());
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
            w!(format!("type '{stype}' unknown, skipped"));
            return None;
        }
    };
    if command.is_empty() {
        w!("no command, skipped".to_string());
        return None;
    }

    // Shell assembly (same comments as dinit-import.sh).
    let mut pre = String::new();
    if !wdir.is_empty() {
        pre.push_str(&format!("cd {} && ", squote(&wdir)));
    }
    if !notifyfd.is_empty() {
        if zt == "oneshot" || zt == "forking" {
            w!("pipefd needs a plain process, ignored".to_string());
            notifyfd.clear();
        } else if command.contains("--print-address") {
            // dbus-style print-and-exit: the flag prints the bus address
            // and the daemon is gone (verified: exit 0, nothing left to
            // supervise). A supervisor cannot use that for anything, so
            // drop the flag (and the dance, which only existed to feed
            // it) and run the daemon foreground with no handshake.
            command = strip_print_address(&command);
            w!("--print-address dropped: print-and-exit cannot supervise; daemon runs foreground with ready = none".to_string());
            ready = "none".to_string();
            notifyfd.clear();
        } else {
            pre.push_str(&format!("exec {notifyfd}>&$ZINIT_NOTIFY_FD; "));
        }
    }
    if !notifyvar.is_empty() {
        if zt == "oneshot" || zt == "forking" {
            w!("pipevar needs a plain process, ignored".to_string());
            notifyvar.clear();
        } else {
            pre.push_str(&format!("export {notifyvar}=\"$ZINIT_NOTIFY_FD\"; "));
        }
    }
    // dinit activation never comes under supervision: a dinit-patched
    // daemon waiting on it (dbus-daemon --dinit) binds nothing and hangs
    // its whole subtree (verified live: empty /run/dbus, dinit-dbus
    // stuck). Strip from both command twins, before quote selection.
    {
        let mut dropped = false;
        for twin in [&mut command, &mut command_raw] {
            if twin.contains("--dinit") {
                let stripped = strip_dinit_flag(twin);
                if stripped != *twin {
                    *twin = stripped;
                    dropped = true;
                }
            }
        }
        if dropped {
            w!("--dinit dropped: no dinit activation under supervision; daemon binds itself".to_string());
        }
    }
    // pass-cs-fd becomes an explicit socket path: compatd listens on its
    // default, and dinit-dbus -S connects there instead of waiting on an
    // fd nobody passes (verified live: FD-less dinit-dbus exits at once).
    if pass_cs_fd {
        for twin in [&mut command, &mut command_raw] {
            twin.push_str(" -S ");
            twin.push_str(DINIT_SOCK_DEFAULT);
        }
        w!(format!(
            "pass-cs-fd mapped to -S {} (compatd default)",
            DINIT_SOCK_DEFAULT
        ));
    }
    let mut stunt = String::new();
    if !stopcmd_raw.is_empty() {
        stunt = format!("trap {} TERM; ", squote(&stopcmd_raw));
    }
    let mut tail = String::new();
    if !chain.is_empty() {
        if !valid_name(&chain) {
            w!(format!("chain-to {chain} ignored"));
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
    let quoted = command_raw.contains('"');
    if quoted {
        if zt == "process" {
            command = command_raw.clone();
        } else {
            w!("quoted command needs a shell; oneshot/forking kept direct and may split it wrongly");
        }
    }
    let mut zt_owned = zt.to_string();
    if quoted && zt == "process" {
        // Raw shell text, quotes intact: the shell groups exactly like
        // dinit's own tokenizer did (verified: zero quoted commands in
        // the 63-service Chimera corpus, so this path is the exception).
        // Console loss (if requested) is reported once, below.
        zt_owned = "script".to_string();
        if !stunt.is_empty() || !tail.is_empty() {
            command = format!("{stunt}{pre}{command}{tail}");
        } else {
            command = format!("{pre}exec {command}");
        }
    } else if !pre.is_empty() || !stunt.is_empty() || !tail.is_empty() {
        if zt == "forking" || zt == "oneshot" {
            if !wdir.is_empty() {
                w!("working-dir needs a shell, ignored".to_string());
            }
            if !stopcmd.is_empty() {
                w!("stop-command needs a shell, ignored".to_string());
            }
            if !chain.is_empty() {
                w!("chain-to needs a shell, ignored".to_string());
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
    if run_console {
        // Only a directly-executed process can take a console: anything
        // the shell must parse (quotes, cd/trap/chain preludes) keeps its
        // shell and loses the console, loudly. There is no console+shell
        // kind to map to.
        if zt == "process" && !quoted && pre.is_empty() && stunt.is_empty() && tail.is_empty() {
            zt_owned = "console".to_string();
            extra_tty = "tty = /dev/console".to_string();
        } else if zt_owned == "process" || zt_owned == "script" {
            w!("console lost: the command needs a shell, which has no console");
        } else {
            w!("console option needs a process service, ignored".to_string());
        }
    }
    if zt_owned == "oneshot" && restart == "on-failure" {
        restart = "never".to_string();
    }
    // dinit restarts bounded 3-per-10s by default (delay 0.2s); explicit
    // limit keys narrow it, count 0 lifts it (unbounded).
    let envelope = || {
        if rcount == "0" {
            String::new()
        } else {
            format!(
                "{} restarts per {}",
                if rcount.is_empty() { "3" } else { rcount.as_str() },
                if rinterval.is_empty() { "10s" } else { rinterval.as_str() }
            )
        }
    };
    if restart == "always" {
        budget = envelope();
    } else {
        match zt_owned.as_str() {
            "process" | "forking" | "script" | "console" => {
                if restart.is_empty() {
                    restart = "always".to_string();
                    budget = envelope();
                }
            }
            _ => {}
        }
    }
    if export_svcname {
        extra_env.push(format!("env = DINIT_SERVICE={n}"));
    }
    if export_passwd {
        let who = if user.is_empty() { "root" } else { user.as_str() };
        match passwd_entry(who) {
            Some((nm, uid, gid, home, shell)) => {
                extra_env.push(format!("env = USER={nm}"));
                extra_env.push(format!("env = LOGNAME={nm}"));
                extra_env.push(format!("env = HOME={home}"));
                extra_env.push(format!("env = SHELL={shell}"));
                extra_env.push(format!("env = UID={uid}"));
                extra_env.push(format!("env = GID={gid}"));
            }
            None => w!(format!("export-passwd-vars: user {who} not found")),
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
    if !extra_tty.is_empty() {
        o.push_str(&format!("{extra_tty}\n"));
    }
    if !deps.is_empty() {
        o.push_str(&format!("depends = {deps}\n"));
    }
    if !restart.is_empty() {
        o.push_str(&format!("restart = {restart}\n"));
    }
    if !budget.is_empty() {
        o.push_str(&format!("restart-budget = {budget}\n"));
    }
    if !rdelay.is_empty() {
        o.push_str(&format!("restart-delay = {rdelay}\n"));
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
    for e in &extra_env {
        o.push_str(&format!("{e}\n"));
    }
    if !log.is_empty() {
        o.push_str(&format!("log = {log}\n"));
    }
    if !ready.is_empty() {
        o.push_str(&format!("ready = {ready}\n"));
    }
    for e in &extra_net {
        o.push_str(&format!("{e}\n"));
    }
    for e in &extra_rl {
        o.push_str(&format!("{e}\n"));
    }
    if !extra_cg.is_empty() {
        o.push_str(&format!("{extra_cg}\n"));
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
    cmd_convert_dirs(std::slice::from_ref(&in_dir.to_path_buf()), out_dir)
}

/// Convert several dinit dirs into one output with a single shared
/// dependency graph: later dirs override earlier files by name (dinit
/// layering: /etc wins over /usr/lib), and the before-inversion and the
/// boot closure run ONCE over the union. Per-dir converts had split-brain
/// state — each pass disabled whatever its own partial graph could not
/// reach, so the second pass froze services the first had enabled
/// (tmpfiles-setup and friends stayed down and the boot stalled).
fn cmd_convert_dirs(dirs: &[std::path::PathBuf], out_dir: &std::path::Path) -> i32 {
    for d in dirs {
        if !d.is_dir() {
            warn(&format!("{}: not a directory", d.display()));
            return 1;
        }
    }
    if std::fs::create_dir_all(out_dir).is_err() {
        warn("cannot create outdir");
        return 1;
    }
    let mut pulls: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut befores: Vec<(String, String)> = Vec::new();
    let mut converted: Vec<Converted> = Vec::new();
    let mut warns: Vec<String> = Vec::new();
    let mut done: BTreeSet<String> = BTreeSet::new();
    // Reversed so the later (higher-precedence) dir wins each name.
    for in_dir in dirs.iter().rev() {
        let mut names: Vec<String> = Vec::new();
        let rd = match std::fs::read_dir(in_dir) {
            Ok(r) => r,
            Err(_) => {
                warn(&format!("cannot read {}", in_dir.display()));
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
        for n in &names {
            if !valid_name(n) {
                warns.push(format!("{n}: odd name, skipped"));
                continue;
            }
            if !done.insert(n.clone()) {
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
        // The mirror target: `a` is `after:X` (order-only). If X is
        // already listed — required, soft, or order-only — adding it
        // again either duplicates it or, worse, pairs `after:X` with `X`
        // or `opt:X`, which zinit rejects (E010). A required edge already
        // orders the pair, so skipping loses nothing.
        let want = a
            .strip_prefix("after:")
            .or_else(|| a.strip_prefix("opt:"))
            .unwrap_or(a.as_str());
        for l in cur.lines() {
            if l.starts_with("depends = ") {
                let have = l["depends = ".len()..].split(',').any(|e| {
                    let e = e.trim();
                    e.strip_prefix("after:")
                        .or_else(|| e.strip_prefix("opt:"))
                        .unwrap_or(e)
                        == want
                });
                if have {
                    out.push_str(l);
                    out.push('\n');
                } else {
                    out.push_str(&format!("depends = {a}, {}\n", &l["depends = ".len()..]));
                }
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
    // boot closure: BFS from boot over recorded pulls. Any converted
    // boot counts, whatever layer it came from.
    if converted.iter().any(|c| c.name == "boot") {
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
                // Only manage what convert generated (this run) or already
                // managed (carries an `enabled =` line): operator-owned
                // seasoning (net, probe, local services) is left exactly
                // as written, or every convert would disable the machine's
                // own files. A sourceless file we never touched gets one
                // loud warn telling the operator to remove it or pin it
                // with `enabled = no` — visible, reversible, no data loss.
                // Any present `enabled =` line (yes or no) is respected as
                // the operator's pin: appending another is fatal E005.
                let has_enabled = cur.lines().any(|l| {
                    let t = l.trim();
                    t == "enabled = no" || t == "enabled = yes"
                });
                if has_enabled {
                    continue;
                }
                let generated = converted.iter().any(|c| c.name == svc);
                if !generated {
                    warns.push(format!(
                        "{svc}: no dinit source, left alone (remove it or pin enabled = no)"
                    ));
                    continue;
                }
                // Idempotent: re-running convert (or `start`) over its own
                // output must not stack another `enabled = no` — a duplicate
                // directive is fatal E005 at load and bricks the boot.
                {
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

// ---------- dinitctl: the dinitctl(8) command set ----------
//
// Exit codes mirror dinitctl: 0 ok, 1 error/unimplemented, 2 usage.
// Output stays C4-transparent (the reply line prints verbatim) except
// where dinitctl defines its own (is-started/is-failed state words,
// catlog contents, signal --list). Anything dinitctl accepts but zinit
// cannot do either maps to the nearest real action (wake/release/trigger
// start; unpin succeeds vacuously; reload re-converts) or fails loudly:
// unload, shutdown and untrigger have no counterpart by design.

fn resolve_outdir(flag: Option<&str>) -> std::path::PathBuf {
    if let Some(d) = flag {
        return std::path::PathBuf::from(d);
    }
    if let Ok(v) = std::env::var("ZINIT_CONFIG_DIRS") {
        if let Some(last) = v.split(':').filter(|x| !x.is_empty()).last() {
            return std::path::PathBuf::from(last);
        }
    }
    std::path::PathBuf::from("/etc/zinit/services.d")
}

/// dinit source directories: --services-dir repeats, else the system pair
/// (/etc shadows /usr/lib, like dinit's own overlay).
fn dinit_source_dirs(extra: &[String]) -> Vec<std::path::PathBuf> {
    if extra.is_empty() {
        vec![
            std::path::PathBuf::from("/etc/dinit.d"),
            std::path::PathBuf::from("/usr/lib/dinit.d"),
        ]
    } else {
        extra.iter().map(std::path::PathBuf::from).collect()
    }
}

/// Find a service description: exact name, then the `base` of `base@arg`.
fn find_dinit_file(dirs: &[std::path::PathBuf], name: &str) -> Option<std::path::PathBuf> {
    for d in dirs {
        let p = d.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(at) = name.rfind('@') {
        let base = &name[..at];
        if !base.is_empty() {
            for d in dirs {
                let q = d.join(base);
                if q.is_file() {
                    return Some(q);
                }
            }
        }
    }
    None
}

/// The single `waits-for.d:` directory of a service file (dinit requires
/// exactly one for enable/disable to make sense).
fn waits_for_dir(svc_file: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let text = std::fs::read_to_string(svc_file).map_err(|e| format!("cannot read {}: {e}", svc_file.display()))?;
    let mut found = Vec::new();
    for ln in text.lines() {
        let t = trim_spaces(ln);
        for prefix in ["waits-for.d:", "waits-for.d="] {
            if let Some(rest) = t.strip_prefix(prefix) {
                found.push(trim_spaces(rest).to_string());
            }
        }
    }
    if found.len() != 1 {
        return Err(format!("{} has {} waits-for.d dirs, need exactly 1", svc_file.display(), found.len()));
    }
    let dir = std::path::PathBuf::from(&found[0]);
    Ok(if dir.is_absolute() {
        dir
    } else {
        svc_file.parent().unwrap_or(std::path::Path::new(".")).join(dir)
    })
}

fn meta_enable_via(svc_file: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(svc_file).ok()?;
    for ln in text.lines() {
        let t = trim_spaces(ln);
        let t = t.strip_prefix('@')?;
        let mut it = t.split_whitespace();
        if it.next()? != "meta" {
            continue;
        }
        if it.next()? != "enable-via" {
            continue;
        }
        return it.next().map(|x| x.to_string());
    }
    None
}

/// Re-convert the layered source dirs holding a dinit source file, then
/// reload-all (skipped offline, where there is no daemon to tell).
///
/// All layers, never just the parent: a single-dir convert runs the boot
/// closure over a partial graph, and a layer owning its own `boot` file
/// (like /etc/dinit.d) then disables everything outside it — the same
/// decimation `watch` used to do before cmd_convert_dirs.
fn reconvert_and_reload(
    src_file: &std::path::Path,
    dirs: &[std::path::PathBuf],
    outdir: &std::path::Path,
    offline: bool,
) -> bool {
    let parent = match src_file.parent() {
        Some(p) => p,
        None => return false,
    };
    let mut layers: Vec<std::path::PathBuf> = dirs
        .iter()
        .filter(|d| d.is_dir())
        .cloned()
        .collect();
    if !layers.iter().any(|d| d == parent) {
        layers.push(parent.to_path_buf());
    }
    if cmd_convert_dirs(&layers, outdir) != 0 {
        return false;
    }
    if offline {
        return true;
    }
    let sock = zinit_socket(None);
    matches!(c4_ask(&sock, "C dinitctl reload-all\n"), Some(r) if r.starts_with("R "))
}

/// Persist a dependency edge into the generated confs (shared with the
/// compatd ADD_DEP path): REGULAR/MILESTONE are hard, WAITS_FOR is soft.
fn dep_add_out(outdir: &std::path::Path, zsock: &str, from: &str, to: &str, dtype: u8) -> bool {
    // Names ride the wire; a traversal here writes outside the services
    // dir. Both ends must be plain service names (instances included).
    if !valid_name(from) || !valid_name(to) {
        return false;
    }
    let tok = match dep_token(dtype, to) {
        Some(x) => x,
        None => return false,
    };
    // Never write an edge to a ghost: the supervisor fatals the whole
    // boot on an unknown required dep, so a dangling write bricks the
    // machine on next reload (verified: dinit-dbus pinned an
    // unmaterialized agetty-service@ttyS0 and agetty took the boot
    // down with it). Synthesize base@arg from the base template when
    // present, else refuse (NAK).
    if !outdir.join(format!("{to}.conf")).is_file() {
        let Some(at) = to.rfind('@') else {
            return false;
        };
        let (base, arg) = (&to[..at], &to[at + 1..]);
        if base.is_empty() || arg.is_empty() {
            return false;
        }
        let Ok(t) = std::fs::read_to_string(outdir.join(format!("{base}.conf"))) else {
            return false;
        };
        if std::fs::write(outdir.join(format!("{to}.conf")), t).is_err() {
            return false;
        }
    }
    let mut deps = match read_depends(outdir, from) {
        Some(d) => d,
        None => return false,
    };
    if deps.contains(&tok) {
        return true;
    }
    if dtype == 0 {
        if let (Some(fs), Some(ts)) = (z_status(zsock, from), z_status(zsock, to)) {
            if fs.state != ST_STOPPED && ts.state != ST_STARTED {
                return false;
            }
        }
    }
    if would_cycle(outdir, from, to) {
        return false;
    }
    deps.push(tok);
    if !write_depends(outdir, from, &deps) {
        return false;
    }
    matches!(c4_ask(zsock, "C dinitctl reload-all\n"), Some(r) if r.starts_with("R "))
}

fn dep_remove_out(outdir: &std::path::Path, zsock: &str, from: &str, to: &str, dtype: u8) -> bool {
    let tok = match dep_token(dtype, to) {
        Some(x) => x,
        None => return false,
    };
    let mut deps = match read_depends(outdir, from) {
        Some(d) => d,
        None => return false,
    };
    if !deps.contains(&tok) {
        return false;
    }
    deps.retain(|d| d != &tok);
    if !write_depends(outdir, from, &deps) {
        return false;
    }
    matches!(c4_ask(zsock, "C dinitctl reload-all\n"), Some(r) if r.starts_with("R "))
}

fn signal_number(id: &str) -> Option<i32> {
    if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) {
        return id.parse::<i32>().ok().filter(|&n| (1..=64).contains(&n));
    }
    let name = id.strip_prefix("SIG_").or_else(|| id.strip_prefix("SIG")).unwrap_or(id);
    let table: &[(&str, i32)] = &[
        ("HUP", 1), ("INT", 2), ("QUIT", 3), ("ILL", 4), ("TRAP", 5), ("ABRT", 6),
        ("BUS", 7), ("FPE", 8), ("KILL", 9), ("USR1", 10), ("SEGV", 11), ("USR2", 12),
        ("PIPE", 13), ("ALRM", 14), ("TERM", 15), ("STKFLT", 16), ("CHLD", 17),
        ("CONT", 18), ("STOP", 19), ("TSTP", 20), ("TTIN", 21), ("TTOU", 22),
        ("URG", 23), ("XCPU", 24), ("XFSZ", 25), ("VTALRM", 26), ("PROF", 27),
        ("WINCH", 28), ("IO", 29), ("PWR", 30), ("SYS", 31),
    ];
    for (n, v) in table {
        if name.eq_ignore_ascii_case(n) {
            return Some(*v);
        }
    }
    if let Some(rest) = name.strip_prefix("RTMIN+") {
        if let Ok(k) = rest.parse::<i32>() {
            if (0..=30).contains(&k) {
                return Some(34 + k);
            }
        }
    }
    if name.eq_ignore_ascii_case("RTMIN") {
        return Some(34);
    }
    if name.eq_ignore_ascii_case("RTMAX") {
        return Some(64);
    }
    None
}

fn wait_state(sock: &str, name: &str, want_running: bool, polls: u32) -> bool {
    for _ in 0..polls {
        match z_status(sock, name) {
            Some(st) if (st.state == ST_STARTED) == want_running => return true,
            None => return false,
            _ => std::thread::sleep(std::time::Duration::from_millis(200)),
        }
    }
    z_status(sock, name).map(|st| (st.state == ST_STARTED) == want_running).unwrap_or(false)
}

fn state_word(sock: &str, name: &str) -> Option<&'static str> {
    match z_status(sock, name) {
        Some(st) => Some(match st.state {
            1 => "STARTING",
            2 => "STARTED",
            3 => "STOPPING",
            _ => "STOPPED",
        }),
        None => None,
    }
}

#[derive(Default)]
struct DctlOpts {
    socket: Option<String>,
    user_sock: bool,
    quiet: bool,
    no_wait: bool,
    pin: bool,
    force: bool,
    ignore_unstarted: bool,
    clear: bool,
    offline: bool,
    sdirs: Vec<String>,
    from: Option<String>,
    out: Option<String>,
    list_flag: bool,
}

fn dctl_usage() -> i32 {
    eprintln!("usage: dinitctl [options] <start|stop|status|list|restart|wake|release|unpin|unload|reload|shutdown|add-dep|rm-dep|enable|disable|trigger|untrigger|setenv|unsetenv|is-started|is-failed|catlog|signal> [...]");
    1
}

fn cmd_dinitctl(args: &[String]) -> i32 {
    cmd_dinitctl_sock(args, None)
}

fn cmd_dinitctl_sock(args: &[String], sock_override: Option<&str>) -> i32 {
    let mut o = DctlOpts::default();
    let mut words: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--use-passed-cfd" => {}
            "--" => {
                words.extend_from_slice(&args[i + 1..]);
                break;
            }
            "--socket-path" | "-p" => {
                i += 1;
                match args.get(i) {
                    Some(v) => o.socket = Some(v.clone()),
                    None => return dctl_usage(),
                }
            }
            "--system" | "-s" => {}
            "--user" | "-u" => o.user_sock = true,
            "--quiet" => o.quiet = true,
            "--no-wait" => o.no_wait = true,
            "--pin" => o.pin = true,
            "--force" => o.force = true,
            "--ignore-unstarted" => o.ignore_unstarted = true,
            "--clear" => o.clear = true,
            "--offline" | "-o" => o.offline = true,
            "--services-dir" | "-d" => {
                i += 1;
                match args.get(i) {
                    Some(v) => o.sdirs.push(v.clone()),
                    None => return dctl_usage(),
                }
            }
            "--from" => {
                i += 1;
                match args.get(i) {
                    Some(v) => o.from = Some(v.clone()),
                    None => return dctl_usage(),
                }
            }
            "--out" => {
                i += 1;
                match args.get(i) {
                    Some(v) => o.out = Some(v.clone()),
                    None => return dctl_usage(),
                }
            }
            "--list" | "-l" => o.list_flag = true,
            "--help" | "-h" => {
                dctl_usage();
                return 0;
            }
            "--version" => {
                println!("z-dinit-generator {VERSION} (dinitctl compat)");
                return 0;
            }
            s if s.starts_with('-') => {
                warn(&format!("unknown flag {}", args[i]));
                return 1;
            }
            _ => words.push(args[i].clone()),
        }
        i += 1;
    }
    if !o.sdirs.is_empty() && !o.offline {
        warn("--services-dir is ignored without --offline");
    }
    if std::env::var_os("DINIT_SOCKET_PATH").is_some() && !o.quiet {
        warn("DINIT_SOCKET_PATH is ignored; the C4 socket is --socket-path/$ZINIT_SOCKET");
    }
    if words.is_empty() {
        return dctl_usage();
    }
    // The socket to dial: explicit flag, then environment, then default.
    // --user selects the per-user default instead of the system one.
    let sock = match o.socket.clone().or_else(|| sock_override.map(|x| x.to_string())) {
        Some(p) => p,
        None => match std::env::var("ZINIT_SOCKET") {
            Ok(v) => v,
            Err(_) if o.user_sock => {
                let base = std::env::var("XDG_RUNTIME_DIR")
                    .or_else(|_| std::env::var("HOME").map(|h| format!("{h}/.local/run")))
                    .unwrap_or_else(|_| String::from("/run"));
                format!("{base}/zinit/ctl")
            }
            Err(_) => DEFAULT_SOCKET.to_string(),
        },
    };
    let outdir = resolve_outdir(o.out.as_deref());
    let qwarn = |m: String| {
        if !o.quiet {
            warn(&m);
        }
    };
    let cmd = words[0].clone();
    let rest = &words[1..];
    // One C4 frame out, one reply line back; errors always print.
    let passthrough = |verb: &str, name: &str| -> Result<String, String> {
        if name.chars().any(char::is_whitespace) {
            return Err(String::from("name with whitespace cannot cross C4"));
        }
        c4_transact(&sock, &format!("C dinitctl {verb} {name}\n"))
    };
    // start/stop-family with dinitctl waiting semantics (wait by default).
    let run_action = |verb: &str, name: &str, want_running: bool| -> i32 {
        match passthrough(verb, name) {
            Ok(reply) => {
                if !o.quiet {
                    print!("{reply}");
                }
                if reply.starts_with("R ") {
                    if o.pin {
                        qwarn(format!("--pin has no effect here; {name} is simply {verb}ed"));
                    }
                    if o.force {
                        qwarn("--force has no extra effect here; stops already cascade".to_string());
                    }
                    if !o.no_wait && !wait_state(&sock, name, want_running, 300) {
                        qwarn(format!("{name} did not reach the target state in time"));
                        return 1;
                    }
                    0
                } else if o.ignore_unstarted {
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
    };
    match cmd.as_str() {
        "setenv" | "unsetenv" => {
            // Accepted no-ops (no activation environment under zinit).
            // setenv NAME (bare) would read our own environment upstream;
            // there is nowhere to export it to, so it is announced too.
            qwarn(format!("{} {} (no activation environment under zinit; ignored)", cmd, rest.join(" ")));
            0
        }
        "status" | "list" => {
            if rest.iter().any(|x| x.chars().any(char::is_whitespace)) {
                warn("name with whitespace cannot cross C4".to_string());
                return 1;
            }
            let mut frame = format!("C dinitctl {cmd}");
            for r in rest {
                frame.push(' ');
                frame.push_str(r);
            }
            frame.push('\n');
            match c4_transact(&sock, &frame) {
                Ok(reply) => {
                    if !o.quiet {
                        print!("{reply}");
                    }
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
        "start" | "wake" | "trigger" => {
            if rest.len() != 1 {
                return dctl_usage();
            }
            // wake/trigger mark nothing here: every start is explicit and
            // every oneshot fires at once (documented collapse).
            run_action("start", &rest[0], true)
        }
        "stop" | "release" => {
            if rest.len() != 1 {
                return dctl_usage();
            }
            run_action("stop", &rest[0], false)
        }
        "restart" => {
            if rest.len() != 1 {
                return dctl_usage();
            }
            run_action("restart", &rest[0], true)
        }
        "is-started" => {
            if rest.len() != 1 {
                return dctl_usage();
            }
            match state_word(&sock, &rest[0]) {
                Some(w) => {
                    if !o.quiet {
                        println!("{w}");
                    }
                    if w == "STARTED" {
                        0
                    } else {
                        1
                    }
                }
                None => {
                    qwarn(format!("{}: unknown service", rest[0]));
                    1
                }
            }
        }
        "is-failed" => {
            if rest.len() != 1 {
                return dctl_usage();
            }
            match z_status(&sock, &rest[0]) {
                Some(st) => {
                    let word = match st.state {
                        1 => "STARTING",
                        2 => "STARTED",
                        3 => "STOPPING",
                        _ => "STOPPED",
                    };
                    if !o.quiet {
                        println!("{word}");
                    }
                    if st.state == ST_STOPPED && st.exhausted {
                        0
                    } else {
                        1
                    }
                }
                None => {
                    qwarn(format!("{}: unknown service", rest[0]));
                    1
                }
            }
        }
        "unpin" => {
            if rest.len() != 1 {
                return dctl_usage();
            }
            // Nothing is ever pinned: always a success.
            0
        }
        "unload" => {
            if rest.len() != 1 {
                return dctl_usage();
            }
            warn("unload: services live in files, there is nothing transient to unload".to_string());
            1
        }
        "shutdown" => {
            if !rest.is_empty() {
                return dctl_usage();
            }
            // dinitctl shutdown stops everything and powers off.
            if graceful_shutdown(&sock, "/proc/sysrq-trigger", Some(b'o')) {
                0
            } else {
                warn("shutdown failed".to_string());
                1
            }
        }
        "untrigger" => {
            if rest.len() != 1 {
                return dctl_usage();
            }
            warn("untrigger: delayed start has no counterpart; use stop".to_string());
            1
        }
        "reload" => {
            if rest.len() != 1 {
                return dctl_usage();
            }
            let dirs = dinit_source_dirs(&o.sdirs);
            match find_dinit_file(&dirs, &rest[0]) {
                Some(f) => {
                    if reconvert_and_reload(&f, &dirs, &outdir, o.offline) {
                        0
                    } else {
                        warn(format!("reload {} failed", rest[0]));
                        1
                    }
                }
                None => {
                    qwarn(format!("{}: unknown service", rest[0]));
                    1
                }
            }
        }
        "add-dep" | "rm-dep" => {
            if rest.len() != 3 {
                return dctl_usage();
            }
            let dtype = match rest[0].as_str() {
                "need" => 0,
                "regular" => {
                    qwarn("regular is a deprecated alias of need".to_string());
                    0
                }
                "milestone" => 3,
                "waits-for" => 2,
                _ => return dctl_usage(),
            };
            let ok = if cmd == "add-dep" {
                dep_add_out(&outdir, &sock, &rest[1], &rest[2], dtype)
            } else {
                dep_remove_out(&outdir, &sock, &rest[1], &rest[2], dtype)
            };
            if ok {
                0
            } else {
                warn(format!("{} {} -> {} refused", cmd, rest[1], rest[2]));
                1
            }
        }
        "enable" | "disable" => {
            // enable [--from F] T / disable [--from F] T, over the dinit
            // source dirs (offline = files only, no daemon contact).
            if rest.len() != 1 {
                return dctl_usage();
            }
            let to = rest[0].clone();
            let dirs = dinit_source_dirs(&o.sdirs);
            let to_file = match find_dinit_file(&dirs, &to) {
                Some(f) => f,
                None => {
                    qwarn(format!("{to}: unknown service"));
                    return 1;
                }
            };
            let from = match o.from.clone() {
                Some(f) => f,
                None => meta_enable_via(&to_file).unwrap_or_else(|| String::from("boot")),
            };
            let dir = match waits_for_dir(&to_file) {
                Ok(d) => d,
                Err(e) => {
                    warn(&e);
                    return 1;
                }
            };
            if cmd == "enable" {
                if std::fs::create_dir_all(&dir).is_err() {
                    warn(format!("cannot create {}", dir.display()));
                    return 1;
                }
                let link = dir.join(&from);
                if !link.exists() {
                    let target = find_dinit_file(&dirs, &from).unwrap_or_else(|| std::path::PathBuf::from(&from));
                    if std::os::unix::fs::symlink(&target, &link).is_err() {
                        warn(format!("cannot link {}", link.display()));
                        return 1;
                    }
                }
            } else if std::fs::remove_file(dir.join(&from)).is_err() {
                qwarn(format!("{from} is not enabled on {to}"));
                return 1;
            }
            if reconvert_and_reload(&to_file, &dirs, &outdir, o.offline) {
                0
            } else {
                warn(format!("{cmd} {from} -> {to} failed"));
                1
            }
        }
        "catlog" => {
            if rest.len() != 1 {
                return dctl_usage();
            }
            let conf = outdir.join(format!("{}.conf", rest[0]));
            let path = match std::fs::read_to_string(&conf).ok().and_then(|text| {
                text.lines().find_map(|l| l.strip_prefix("log = file:").map(|x| x.to_string()))
            }) {
                Some(p) => p,
                None => {
                    qwarn(format!("{}: no file log", rest[0]));
                    return 1;
                }
            };
            match std::fs::read_to_string(&path) {
                Ok(data) => {
                    print!("{data}");
                    if o.clear && std::fs::write(&path, "").is_err() {
                        warn(format!("cannot clear {path}"));
                        return 1;
                    }
                    0
                }
                Err(e) => {
                    warn(&format!("cannot read {path}: {e}"));
                    1
                }
            }
        }
        "signal" => {
            if o.list_flag && rest.is_empty() {
                println!("HUP INT QUIT ILL TRAP ABRT BUS FPE KILL USR1 SEGV USR2 PIPE ALRM TERM STKFLT CHLD CONT STOP TSTP TTIN TTOU URG XCPU XFSZ VTALRM PROF WINCH IO PWR SYS RTMIN-RTMAX");
                return 0;
            }
            if rest.len() != 2 {
                return dctl_usage();
            }
            let signum = match signal_number(&rest[0]) {
                Some(n) => n,
                None => {
                    warn(format!("unknown signal {}", rest[0]));
                    return 1;
                }
            };
            match z_status(&sock, &rest[1]).and_then(|st| st.has_pid.then_some(st.pid)) {
                Some(pid) => {
                    match std::process::Command::new("kill")
                        .arg("-s")
                        .arg(signum.to_string())
                        .arg(pid.to_string())
                        .status()
                    {
                        Ok(st) if st.success() => 0,
                        _ => {
                            warn(format!("signal not delivered to {}", rest[1]));
                            1
                        }
                    }
                }
                None => {
                    qwarn(format!("{}: no running process", rest[1]));
                    1
                }
            }
        }
        _ => {
            warn(&format!("unknown command {cmd}"));
            1
        }
    }
}

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

// ---------- compatd: full dinit control-protocol server ----------//
// Speaks protocol v7 (dinit 0.20, control-cmds.h) over /run/dinitctl so
// devmon, dinitctl and libdinitctl clients work unchanged. Every command
// is either served for real through zinit C4 or refused with the same
// reply dinit itself would send:
//
// * state queries (FIND/LOAD/STATUS/LIST/NAME) read live zinit state;
//   unknown services get NOSERVICE, bad handles NAK, malformed packets
//   BADREQ + close, exactly like control.cc.
// * START/STOP/WAKE/RELEASE drive `zctl start/stop/restart`; already-there
//   answers ALREADYSS, failures NAK, and a state-change emits both event
//   packets (SERVICEEVENT + SERVICEEVENT5) so `dinitctl --wait` works.
// * ADD/REM/ENABLE (+V7) persist into the generated .conf files and
//   `reload-all` (dependencies are file-bound under zinit); removal of a
//   missing edge is NAK, cycles are NAKed before they can break a boot.
// * No counterpart exists for pin/unload/shutdown/trigger/signal/env:
//   UNPIN ACKs (nothing is ever pinned), RELOAD maps to `reload-all`,
//   SETENV/LISTENENV are accepted no-ops with an empty environment
//   (matches the dinitctl shim), everything else gets the honest NAK.
//   WAKE starts unconditionally — real WAKE needs a started dependent,
//   which is invisible without the plan (documented divergence, and what
//   keeps devmon quiet).
//
// No dependencies, no Cargo project: `rustc -O --edition=2021`.
// Usage: compatd [/run/dinitctl] [--out DIR]. Exit nonzero only when it
// cannot bind.

/// Live view of one service, parsed from `zctl status` output.
struct ZStatus {
    state: u8,
    target: u8,
    pid: i32,
    has_pid: bool,
    exhausted: bool,
}

fn c4_dial(sock: &str) -> Option<BufReader<UnixStream>> {
    let io = UnixStream::connect(sock).ok()?;
    let _ = io.set_read_timeout(Some(std::time::Duration::from_secs(2)));
    Some(BufReader::new(io))
}

fn c4_ask(sock: &str, frame: &str) -> Option<String> {
    let mut io = c4_dial(sock)?;
    io.get_mut().write_all(frame.as_bytes()).ok()?;
    let mut reply = String::new();
    io.read_line(&mut reply).ok()?;
    Some(reply)
}

fn parse_status(reply: &str) -> Option<ZStatus> {
    // `R <id> 0 <name> <state> desired=<up|down> pid=<n|-> up=... restarts=a/b`
    let t: Vec<&str> = reply.split_whitespace().collect();
    if t.len() < 8 || t[0] != "R" {
        return None;
    }
    let state = match t[4] {
        "running" => ST_STARTED,
        "starting" => 1,
        "stopping" => 3,
        _ => ST_STOPPED,
    };
    let target = if t[5] == "desired=up" { ST_STARTED } else { ST_STOPPED };
    let (pid, has_pid) = match t[6].strip_prefix("pid=") {
        Some("-") | None => (-1, false),
        Some(n) => (n.parse().unwrap_or(-1), true),
    };
    let (cur, max) = t
        .iter()
        .find(|w| w.starts_with("restarts="))
        .and_then(|w| w["restarts=".len()..].split_once('/'))
        .and_then(|(a, b)| Some((a.parse::<u64>().ok()?, b.parse::<u64>().ok()?)))
        .unwrap_or((0, 0));
    // ponytail: exhausted-loop heuristic; true exit reasons need a supervisor channel.
    let exhausted = max > 0 && cur >= max;
    Some(ZStatus {
        state,
        target,
        pid,
        has_pid,
        exhausted,
    })
}

fn z_status(sock: &str, name: &str) -> Option<ZStatus> {
    if name.chars().any(char::is_whitespace) {
        return None;
    }
    parse_status(&c4_ask(sock, &format!("C compatd status {name}\n"))?)
}

fn z_list(sock: &str) -> Option<Vec<(String, String)>> {
    // `R <id> 0 <N> <name>=<state> ...`
    let reply = c4_ask(sock, "C compatd list\n")?;
    let t: Vec<&str> = reply.split_whitespace().collect();
    if t.len() < 4 || t[0] != "R" {
        return None;
    }
    let mut out = Vec::new();
    for w in &t[4..] {
        if let Some((n, s)) = w.split_once('=') {
            out.push((n.to_string(), s.to_string()));
        }
    }
    Some(out)
}

fn z_act(sock: &str, verb: &str, name: &str) -> bool {
    if name.chars().any(char::is_whitespace) {
        return false;
    }
    match c4_ask(sock, &format!("C compatd {verb} {name}\n")) {
        Some(r) => r.starts_with("R "),
        None => false,
    }
}

fn z_state_tok(tok: &str) -> u8 {
    match tok {
        "running" => ST_STARTED,
        "starting" => 1,
        "stopping" => 3,
        _ => ST_STOPPED,
    }
}

/// 10-byte v1-4 status buffer (control.cc fill_status_buffer, LP64).
fn status_buf(st: &ZStatus) -> Vec<u8> {
    let mut b = vec![0u8; 10];
    b[0] = st.state;
    b[1] = st.target;
    if st.has_pid {
        b[2] = 16;
    }
    // ponytail: stop reason is a heuristic (exhausted loop => FAILED), the
    // supervisor exposes no EXECFAILED/TIMEDOUT channel to ask through.
    b[3] = if st.exhausted { 3 } else { 0 };
    if st.has_pid {
        b[6..10].copy_from_slice(&st.pid.to_le_bytes());
    }
    b
}

/// 14-byte v5 buffer: si_code + si_status instead of one pid/exit word.
fn status_buf5(st: &ZStatus) -> Vec<u8> {
    let mut b = vec![0u8; 14];
    b[0] = st.state;
    b[1] = st.target;
    if st.has_pid {
        b[2] = 16;
    }
    b[3] = if st.exhausted { 3 } else { 0 };
    if st.has_pid {
        b[6..10].copy_from_slice(&st.pid.to_le_bytes());
    }
    b
}

/// 30-byte v6 buffer: v5 + conf mtime as timespec (genuinely ours: the
/// descriptions live in files, so the modification time is exact).
fn status_buf6(st: &ZStatus, mtime: (i64, i64)) -> Vec<u8> {
    let mut b = status_buf5(st);
    b.extend_from_slice(&mtime.0.to_le_bytes());
    b.extend_from_slice(&mtime.1.to_le_bytes());
    b
}

fn conf_mtime(outdir: &std::path::Path, name: &str) -> (i64, i64) {
    let md = std::fs::metadata(outdir.join(format!("{name}.conf")));
    let t = md
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
    match t {
        Some(d) => (d.as_secs() as i64, d.subsec_nanos() as i64),
        None => (0, 0),
    }
}

fn emit_events(s: &mut UnixStream, handle: u32, ev: u8, st: &ZStatus) -> bool {
    // Both generations, like control.cc: [100,len17,h,ev,s10] + [101,len21,h,ev,s14].
    let s10 = status_buf(st);
    let s5 = status_buf5(st);
    let mut p1 = Vec::with_capacity(17);
    p1.push(SERVICEEVENT);
    p1.push(17);
    p1.extend_from_slice(&handle.to_le_bytes());
    p1.push(ev);
    p1.extend_from_slice(&s10);
    let mut p5 = Vec::with_capacity(21);
    p5.push(SERVICEEVENT5);
    p5.push(21);
    p5.extend_from_slice(&handle.to_le_bytes());
    p5.push(ev);
    p5.extend_from_slice(&s5);
    wr_all(s, &p1) && wr_all(s, &p5)
}

/// dinit name check for the wire (load-service.h): only empty and a
/// leading '.'/'@' are rejected — the loop after it is dead code upstream,
/// so anything else (including '+', "..", a second '@') is accepted.
fn valid_proto_name(n: &[u8]) -> bool {
    !n.is_empty() && n[0] != b'.' && n[0] != b'@'
}

#[derive(Clone)]
struct Ctx {
    outdir: std::path::PathBuf,
    zsock: String,
    sysrq: String,
}

fn read_depends(outdir: &std::path::Path, from: &str) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(outdir.join(format!("{from}.conf"))).ok()?;
    for l in text.lines() {
        if let Some(rest) = l.strip_prefix("depends = ") {
            return Some(rest.split(", ").map(|s| s.to_string()).collect());
        }
    }
    Some(Vec::new())
}

fn write_depends(outdir: &std::path::Path, from: &str, deps: &[String]) -> bool {
    let p = outdir.join(format!("{from}.conf"));
    let cur = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(_) => return false,
    };
    let mut out = String::new();
    let mut hit = false;
    for l in cur.lines() {
        if l.starts_with("depends = ") {
            if deps.is_empty() {
                hit = true;
                continue;
            }
            out.push_str(&format!("depends = {}\n", deps.join(", ")));
            hit = true;
        } else {
            out.push_str(l);
            out.push('\n');
        }
    }
    if !hit && !deps.is_empty() {
        out.push_str(&format!("depends = {}\n", deps.join(", ")));
    }
    std::fs::write(&p, out).is_ok()
}

/// Would `from -> to` close a dependency cycle? BFS over the generated
/// confs from `to` looking for `from` (any edge kind: zinit refuses to
/// boot on any cycle, so any cycle is a NAK here).
fn would_cycle(outdir: &std::path::Path, from: &str, to: &str) -> bool {
    let mut seen = BTreeMap::<String, bool>::new();
    let mut work = vec![to.to_string()];
    while let Some(n) = work.pop() {
        if n == from {
            return true;
        }
        if seen.contains_key(&n) {
            continue;
        }
        seen.insert(n.clone(), true);
        let base = n.split('@').next().unwrap_or(&n).to_string();
        if let Some(ds) = read_depends(outdir, &base) {
            for d in ds {
                let target = d
                    .strip_prefix("opt:")
                    .or_else(|| d.strip_prefix("after:"))
                    .unwrap_or(&d)
                    .to_string();
                if !seen.contains_key(&target) {
                    work.push(target);
                }
            }
        }
    }
    false
}

fn dep_token(dtype: u8, to: &str) -> Option<String> {
    // REGULAR 0 and MILESTONE 3 are both hard (milestone only softens
    // after success, which the file cannot express); WAITS_FOR 2 orders
    // without gating.
    match dtype {
        0 | 3 => Some(to.to_string()),
        2 => Some(format!("after:{to}")),
        _ => None,
    }
}

// Serve one client until EOF, error or protocol violation. Handles are
// unique across connections via the shared counter; each connection
// keeps its own handle->name table (like dinit itself).
fn serve_one(
    s: &mut UnixStream,
    counter: std::sync::Arc<std::sync::atomic::AtomicU32>,
    names: &mut BTreeMap<u32, String>,
    ctx: &Ctx,
) {
    // One thread per connection (see cmd_compatd): a silent holder must
    // never wedge the next client, so handles stay unique across threads
    // via the shared counter while each connection keeps its own names.
    // BADREQ closes the connection, like control.cc (bad_conn_close).
    macro_rules! badreq {
        () => {{
            let _ = wr_all(s, &[BADREQ]);
            return;
        }};
    }
    loop {
        let mut t = [0u8; 1];
        if !rd_n(s, &mut t) {
            return;
        }
        match t[0] {
            QUERYVERSION => {
                if !wr_all(s, &VER_REPLY) {
                    return;
                }
            }
            FINDSERVICE | LOADSERVICE => {
                let len = match rd_u16(s) {
                    Some(l) => l,
                    None => return,
                };
                if len == 0 || len > 1021 {
                    badreq!();
                }
                let mut name = vec![0u8; len as usize];
                if !rd_n(s, &mut name) {
                    return;
                }
                if !valid_proto_name(&name) {
                    badreq!();
                }
                let svc = String::from_utf8_lossy(&name).into_owned();
                match z_status(&ctx.zsock, &svc) {
                    Some(st) => {
                        let id = counter
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let mut reply = [SERVICERECORD, 0, 0, 0, 0, 0, 0];
                        reply[1] = st.state;
                        reply[2..6].copy_from_slice(&id.to_le_bytes());
                        reply[6] = st.target;
                        names.insert(id, svc);
                        if !wr_all(s, &reply) {
                            return;
                        }
                    }
                    None => {
                        // No supervisor (tests) → canned STARTED, the old
                        // devmon-quiet fallback. Supervisor up but unknown
                        // service → honest NOSERVICE.
                        if c4_dial(&ctx.zsock).is_some() {
                            // Instance synthesis: `base@arg` materializes
                            // from the `base` template (`%i` expands at load,
                            // so a verbatim copy is the correct instance).
                            let mut served = false;
                            if let Some(at) = svc.rfind('@') {
                                let (base, arg) = (&svc[..at], &svc[at + 1..]);
                                if !base.is_empty() && !arg.is_empty() {
                                    let dst = ctx.outdir.join(format!("{svc}.conf"));
                                    if !dst.exists() {
                                        if let Ok(t) = std::fs::read_to_string(
                                            ctx.outdir.join(format!("{base}.conf")),
                                        ) {
                                            if std::fs::write(&dst, t).is_ok() {
                                                let _ = c4_ask(
                                                    &ctx.zsock,
                                                    "C compatd reload-all\n",
                                                );
                                            }
                                        }
                                    }
                                    if let Some(st) = z_status(&ctx.zsock, &svc) {
                                        let id = counter.fetch_add(
                                            1,
                                            std::sync::atomic::Ordering::SeqCst,
                                        );
                                        let mut reply =
                                            [SERVICERECORD, 0, 0, 0, 0, 0, 0];
                                        reply[1] = st.state;
                                        reply[2..6].copy_from_slice(
                                            &id.to_le_bytes(),
                                        );
                                        reply[6] = st.target;
                                        names.insert(id, svc);
                                        served = wr_all(s, &reply);
                                        if !served {
                                            return;
                                        }
                                    }
                                }
                            }
                            if !served && !wr_all(s, &[NOSERVICE]) {
                                return;
                            }
                        } else {
                            let id = counter
                                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            let mut reply = [SERVICERECORD, 2, 0, 0, 0, 0, 2];
                            reply[2..6].copy_from_slice(&id.to_le_bytes());
                            names.insert(id, svc);
                            if !wr_all(s, &reply) {
                                return;
                            }
                        }
                    }
                }
            }
            STARTSERVICE | STOPSERVICE | WAKESERVICE | RELEASESERVICE => {
                // [cmd, flags, handle]: bit0 pin, bit1 gentle, bit2
                // restart-after-stop, bit7 pre-ack.
                let mut rest = [0u8; 5];
                if !rd_n(s, &mut rest) {
                    return;
                }
                let handle = u32::from_le_bytes([rest[1], rest[2], rest[3], rest[4]]);
                let name = match names.get(&handle).cloned() {
                    Some(n) => n,
                    None => badreq!(),
                };
                if rest[0] & 128 != 0 && !wr_all(s, &[PREACK]) {
                    return;
                }
                // pin (bit0) has no counterpart: start/stop covers it.
                // gentle (bit1) cannot enumerate dependents without the
                // plan, so it proceeds (documented).
                let (verb, target) = match t[0] {
                    STARTSERVICE => ("start", ST_STARTED),
                    WAKESERVICE => ("start", ST_STARTED),
                    RELEASESERVICE => ("stop", ST_STOPPED),
                    _ => {
                        if rest[0] & 4 != 0 {
                            ("restart", ST_STARTED)
                        } else {
                            ("stop", ST_STOPPED)
                        }
                    }
                };
                if !z_act(&ctx.zsock, verb, &name) {
                    if !wr_all(s, &[NAK]) {
                        return;
                    }
                    continue;
                }
                match z_status(&ctx.zsock, &name) {
                    Some(st) if st.state == target => {
                        // ponytail: synchronous event approximation; the real
                        // daemon emits on transition, we emit on arrival.
                        emit_events(s, handle, if target == ST_STARTED { 0 } else { 1 }, &st);
                        if !wr_all(s, &[ALREADYSS]) {
                            return;
                        }
                    }
                    _ => {
                        if !wr_all(s, &[ACK]) {
                            return;
                        }
                    }
                }
            }
            UNPINSERVICE => {
                // No pin exists anywhere: unpinning always succeeds.
                let mut rest = [0u8; 4];
                if !rd_n(s, &mut rest) {
                    return;
                }
                let handle = u32::from_le_bytes(rest);
                if !names.contains_key(&handle) {
                    badreq!();
                }
                if !wr_all(s, &[ACK]) {
                    return;
                }
            }
            LISTSERVICES | LISTSERVICES5 => {
                let v5 = t[0] == LISTSERVICES5;
                let graves = match z_list(&ctx.zsock) {
                    Some(l) => l,
                    None => {
                        if !wr_all(s, &[NAK]) {
                            return;
                        }
                        continue;
                    }
                };
                for (n, stok) in &graves {
                    let st = z_status(&ctx.zsock, n).unwrap_or(ZStatus {
                        state: z_state_tok(stok),
                        target: ST_STOPPED,
                        pid: -1,
                        has_pid: false,
                        exhausted: false,
                    });
                    let buf = if v5 { status_buf5(&st) } else { status_buf(&st) };
                    let nb = n.as_bytes();
                    let nl = nb.len().min(256);
                    let mut pkt = Vec::with_capacity(2 + buf.len() + nl);
                    pkt.push(SVCINFO);
                    pkt.push(nl as u8);
                    pkt.extend_from_slice(&buf);
                    pkt.extend_from_slice(&nb[..nl]);
                    if !wr_all(s, &pkt) {
                        return;
                    }
                }
                if !wr_all(s, &[LISTDONE]) {
                    return;
                }
            }
            UNLOADSERVICE => {
                // Descriptions live in files; nothing transient to unload.
                let mut rest = [0u8; 4];
                if !rd_n(s, &mut rest) {
                    return;
                }
                if !names.contains_key(&u32::from_le_bytes(rest)) {
                    badreq!();
                }
                if !wr_all(s, &[NAK]) {
                    return;
                }
            }
            SHUTDOWN => {
                // Shutdown types: REMAIN 1, HALT 2, POWEROFF 3, REBOOT 4,
                // SOFTREBOOT 5, KEXEC 6. Anything else is BADREQ upstream.
                let mut b = [0u8; 1];
                if !rd_n(s, &mut b) {
                    return;
                }
                let letter = match b[0] {
                    1 => None,
                    2 | 3 => Some(b'o'),
                    4 => Some(b'b'),
                    5 | 6 => {
                        if !wr_all(s, &[NAK]) {
                            return;
                        }
                        continue;
                    }
                    _ => badreq!(),
                };
                if graceful_shutdown(&ctx.zsock, &ctx.sysrq, letter) {
                    if !wr_all(s, &[ACK]) {
                        return;
                    }
                } else if !wr_all(s, &[NAK]) {
                    return;
                }
            }
            ADD_DEP | REM_DEP | ENABLESERVICE | ENABLE_SERVICE_V7 | REM_DEP_V7 => {
                // [cmd, deptype, from32, to32]. Persisted into from.conf
                // + reload-all: dependencies are file-bound under zinit.
                let v7 = t[0] == ENABLE_SERVICE_V7 || t[0] == REM_DEP_V7;
                let adding = t[0] == ADD_DEP || t[0] == ENABLESERVICE || t[0] == ENABLE_SERVICE_V7;
                let mut rest = [0u8; 9];
                if !rd_n(s, &mut rest) {
                    return;
                }
                let from_h = u32::from_le_bytes([rest[1], rest[2], rest[3], rest[4]]);
                let to_h = u32::from_le_bytes([rest[5], rest[6], rest[7], rest[8]]);
                let (from, to) = match (names.get(&from_h).cloned(), names.get(&to_h).cloned()) {
                    (Some(f), Some(tn)) if f != tn => (f, tn),
                    _ => badreq!(),
                };
                let tok = match dep_token(rest[0], &to) {
                    Some(x) => x,
                    None => badreq!(),
                };
                let mut deps = match read_depends(&ctx.outdir, &from) {
                    Some(d) => d,
                    None => {
                        if !wr_all(s, &[NAK]) {
                            return;
                        }
                        continue;
                    }
                };
                if adding {
                    if deps.contains(&tok) {
                        if v7 {
                            let st = z_status(&ctx.zsock, &to).unwrap_or(ZStatus {
                                state: ST_STOPPED,
                                target: ST_STOPPED,
                                pid: -1,
                                has_pid: false,
                                exhausted: false,
                            });
                            let mut pkt = vec![SERVICESTATUSR, 1];
                            pkt.extend_from_slice(&status_buf6(&st, conf_mtime(&ctx.outdir, &to)));
                            if !wr_all(s, &pkt) {
                                return;
                            }
                        } else if !wr_all(s, &[ACK]) {
                            return;
                        }
                        continue;
                    }
                    // dinit refuses REGULAR unless from is STOPPED or to is
                    // STARTED; mirror it (best effort: unknown → proceed).
                    if rest[0] == 0 {
                        if let (Some(fs), Some(ts)) = (z_status(&ctx.zsock, &from), z_status(&ctx.zsock, &to)) {
                            if fs.state != ST_STOPPED && ts.state != ST_STARTED {
                                if !wr_all(s, &[NAK]) {
                                    return;
                                }
                                continue;
                            }
                        }
                    }
                    if would_cycle(&ctx.outdir, &from, &to) {
                        if !wr_all(s, &[NAK]) {
                            return;
                        }
                        continue;
                    }
                    deps.push(tok);
                    if !write_depends(&ctx.outdir, &from, &deps) || c4_ask(&ctx.zsock, "C compatd reload-all\n").map(|r| !r.starts_with("R ")).unwrap_or(true) {
                        if !wr_all(s, &[NAK]) {
                            return;
                        }
                        continue;
                    }
                    if t[0] == ENABLESERVICE || t[0] == ENABLE_SERVICE_V7 {
                        // do_enable: a started dependent pulls the dependency.
                        if let Some(fs) = z_status(&ctx.zsock, &from) {
                            if fs.state == ST_STARTED || fs.state == 1 {
                                z_act(&ctx.zsock, "start", &to);
                            }
                        }
                    }
                    if v7 {
                        let st = z_status(&ctx.zsock, &to).unwrap_or(ZStatus {
                            state: ST_STOPPED,
                            target: ST_STOPPED,
                            pid: -1,
                            has_pid: false,
                            exhausted: false,
                        });
                        let mut pkt = vec![SERVICESTATUSR, 0];
                        pkt.extend_from_slice(&status_buf6(&st, conf_mtime(&ctx.outdir, &to)));
                        if !wr_all(s, &pkt) {
                            return;
                        }
                    } else if !wr_all(s, &[ACK]) {
                        return;
                    }
                } else {
                    let had = deps.contains(&tok);
                    if had {
                        deps.retain(|d| d != &tok);
                        if !write_depends(&ctx.outdir, &from, &deps) || c4_ask(&ctx.zsock, "C compatd reload-all\n").map(|r| !r.starts_with("R ")).unwrap_or(true) {
                            if !wr_all(s, &[NAK]) {
                                return;
                            }
                            continue;
                        }
                    }
                    if v7 {
                        let st = z_status(&ctx.zsock, &to).unwrap_or(ZStatus {
                            state: ST_STOPPED,
                            target: ST_STOPPED,
                            pid: -1,
                            has_pid: false,
                            exhausted: false,
                        });
                        let mut pkt = vec![SERVICESTATUSR, if had { 1 } else { 0 }];
                        pkt.extend_from_slice(&status_buf6(&st, conf_mtime(&ctx.outdir, &to)));
                        if !wr_all(s, &pkt) {
                            return;
                        }
                    } else if !wr_all(s, &[if had { ACK } else { NAK }]) {
                        return;
                    }
                }
            }
            QUERY_LOAD_MECH => {
                // DIRLOAD with our outdir as the single service directory.
                let dir = ctx.outdir.display().to_string();
                let cwd = std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| String::from("/"));
                let db = dir.as_bytes();
                let cb = cwd.as_bytes();
                let total = 2 + 4 + 4 + (4 + cb.len()) + (4 + db.len());
                let mut pkt = Vec::with_capacity(total);
                pkt.push(LOADER_MECH);
                pkt.push(1);
                pkt.extend_from_slice(&(total as u32).to_le_bytes());
                pkt.extend_from_slice(&1u32.to_le_bytes());
                pkt.extend_from_slice(&(cb.len() as u32).to_le_bytes());
                pkt.extend_from_slice(cb);
                pkt.extend_from_slice(&(db.len() as u32).to_le_bytes());
                pkt.extend_from_slice(db);
                if !wr_all(s, &pkt) {
                    return;
                }
            }
            QUERYSERVICENAME => {
                // [cmd, reserved, handle]
                let mut rest = [0u8; 5];
                if !rd_n(s, &mut rest) {
                    return;
                }
                let handle = u32::from_le_bytes([rest[1], rest[2], rest[3], rest[4]]);
                match names.get(&handle) {
                    Some(n) => {
                        let nb = n.as_bytes();
                        let mut pkt = Vec::with_capacity(4 + nb.len());
                        pkt.push(SERVICENAME);
                        pkt.push(0);
                        pkt.extend_from_slice(&(nb.len() as u16).to_le_bytes());
                        pkt.extend_from_slice(nb);
                        if !wr_all(s, &pkt) {
                            return;
                        }
                    }
                    None => {
                        if !wr_all(s, &[NAK]) {
                            return;
                        }
                    }
                }
            }
            RELOADSERVICE => {
                let mut rest = [0u8; 4];
                if !rd_n(s, &mut rest) {
                    return;
                }
                if !names.contains_key(&u32::from_le_bytes(rest)) {
                    badreq!();
                }
                // ponytail: reload-all re-reads everything; per-service
                // reload needs a supervisor channel that does not exist.
                match c4_ask(&ctx.zsock, "C compatd reload-all\n") {
                    Some(r) if r.starts_with("R ") => {
                        if !wr_all(s, &[ACK]) {
                            return;
                        }
                    }
                    _ => {
                        if !wr_all(s, &[NAK]) {
                            return;
                        }
                    }
                }
            }
            SETENV => {
                let len = match rd_u16(s) {
                    Some(l) => l,
                    None => return,
                };
                if len == 0 || len > 1021 {
                    badreq!();
                }
                let mut kv = vec![0u8; len as usize];
                if !rd_n(s, &mut kv) {
                    return;
                }
                // Accepted no-op (no activation environment exists — same
                // contract as the dinitctl shim); malformed is BADREQ.
                match kv.iter().position(|&b| b == b'=') {
                    Some(0) | None => badreq!(),
                    Some(_) => {
                        if !wr_all(s, &[ACK]) {
                            return;
                        }
                    }
                }
            }
            SERVICESTATUSQ | SERVICESTATUS5Q | SERVICESTATUS6Q => {
                let mut rest = [0u8; 4];
                if !rd_n(s, &mut rest) {
                    return;
                }
                let handle = u32::from_le_bytes(rest);
                let name = match names.get(&handle) {
                    Some(n) => n.clone(),
                    None => {
                        if !wr_all(s, &[NAK]) {
                            return;
                        }
                        continue;
                    }
                };
                let st = z_status(&ctx.zsock, &name).unwrap_or(ZStatus {
                    state: ST_STOPPED,
                    target: ST_STOPPED,
                    pid: -1,
                    has_pid: false,
                    exhausted: false,
                });
                let mut pkt = vec![SERVICESTATUSR, 0];
                match t[0] {
                    SERVICESTATUSQ => pkt.extend_from_slice(&status_buf(&st)),
                    SERVICESTATUS5Q => pkt.extend_from_slice(&status_buf5(&st)),
                    _ => pkt.extend_from_slice(&status_buf6(&st, conf_mtime(&ctx.outdir, &name))),
                }
                if !wr_all(s, &pkt) {
                    return;
                }
            }
            SETTRIGGER => {
                // No triggered type survives translation (oneshot never
                // waits), so there is nothing to trigger: NAK, like dinit
                // answers for a non-triggered service.
                let mut rest = [0u8; 5];
                if !rd_n(s, &mut rest) {
                    return;
                }
                if !wr_all(s, &[NAK]) {
                    return;
                }
            }
            CATLOG => {
                // [cmd, flags, handle]: file logs are readable, everything
                // else NAKs (dinit only serves BUFFER logs the same way).
                let mut rest = [0u8; 5];
                if !rd_n(s, &mut rest) {
                    return;
                }
                let handle = u32::from_le_bytes([rest[1], rest[2], rest[3], rest[4]]);
                let data = names
                    .get(&handle)
                    .and_then(|n| std::fs::read_to_string(ctx.outdir.join(format!("{n}.conf"))).ok())
                    .and_then(|text| {
                        text.lines().find_map(|l| {
                            l.strip_prefix("log = file:").map(|p| p.to_string())
                        })
                    })
                    .and_then(|p| std::fs::read(&p).ok());
                match data {
                    Some(all) => {
                        let start = all.len().saturating_sub(255);
                        let tail = &all[start..];
                        let mut pkt = Vec::with_capacity(3 + tail.len());
                        pkt.push(SERVICE_LOG);
                        pkt.push(0);
                        pkt.push(tail.len() as u8);
                        pkt.extend_from_slice(tail);
                        if !wr_all(s, &pkt) {
                            return;
                        }
                    }
                    None => {
                        if !wr_all(s, &[NAK]) {
                            return;
                        }
                    }
                }
            }
            SIGNALQ => {
                // [cmd, signum i32, handle]. Delivered via the POSIX kill
                // utility (std has no kill); missing binary or dead pid is
                // KILLERR, matching the failure a real daemon would report.
                let mut rest = [0u8; 8];
                if !rd_n(s, &mut rest) {
                    return;
                }
                let signum = i32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]);
                let handle = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]);
                let name = match names.get(&handle) {
                    Some(n) => n.clone(),
                    None => {
                        if !wr_all(s, &[NAK]) {
                            return;
                        }
                        continue;
                    }
                };
                if signum < 1 || signum > 64 {
                    if !wr_all(s, &[SIGNAL_BADSIG]) {
                        return;
                    }
                    continue;
                }
                let pid = z_status(&ctx.zsock, &name).and_then(|st| st.has_pid.then_some(st.pid));
                match pid {
                    None => {
                        if !wr_all(s, &[SIGNAL_NOPID]) {
                            return;
                        }
                    }
                    Some(p) => {
                        let ok = std::process::Command::new("kill")
                            .arg("-s")
                            .arg(signum.to_string())
                            .arg(p.to_string())
                            .status()
                            .map(|st| st.success())
                            .unwrap_or(false);
                        if !wr_all(s, &[if ok { ACK } else { SIGNAL_KILLERR }]) {
                            return;
                        }
                    }
                }
            }
            QUERYSERVICEDSCDIR => {
                // [cmd, spare, handle]: spare must be 0.
                let mut rest = [0u8; 5];
                if !rd_n(s, &mut rest) {
                    return;
                }
                let handle = u32::from_le_bytes([rest[1], rest[2], rest[3], rest[4]]);
                if rest[0] != 0 || !names.contains_key(&handle) {
                    if !wr_all(s, &[NAK]) {
                        return;
                    }
                    continue;
                }
                let dir = ctx.outdir.display().to_string();
                let db = dir.as_bytes();
                let mut pkt = Vec::with_capacity(5 + db.len());
                pkt.push(SVCDSCDIR);
                pkt.extend_from_slice(&(db.len() as u32).to_le_bytes());
                pkt.extend_from_slice(db);
                if !wr_all(s, &pkt) {
                    return;
                }
            }
            CLOSEHANDLE => {
                let mut rest = [0u8; 4];
                if !rd_n(s, &mut rest) {
                    return;
                }
                let h = u32::from_le_bytes(rest);
                if names.remove(&h).is_none() {
                    // Unknown handle closes the connection upstream too.
                    badreq!();
                }
                if !wr_all(s, &[ACK]) {
                    return;
                }
            }
            GETALLENV => {
                let mut b = [0u8; 1];
                if !rd_n(s, &mut b) {
                    return;
                }
                if b[0] != 0 {
                    badreq!();
                }
                // Empty store: [ALLENV, size_t 0]. The width follows the
                // build (8 on 64-bit), exactly like control.cc.
                let mut pkt = vec![ALLENV];
                pkt.extend_from_slice(&(0usize).to_le_bytes());
                if !wr_all(s, &pkt) {
                    return;
                }
            }
            LISTENENV => {
                // Accepted; environment never changes, so no ENVEVENT ever
                // follows — the steady state stays silent, as observed
                // against real dinit.
                if !wr_all(s, &[ACK]) {
                    return;
                }
            }
            _ => {
                badreq!();
            }
        }
    }
}

/// Graceful shutdown: stop every non-stopped service (dinit stop_all
/// order), then, unless `letter` is None (REMAIN: stay up, no services),
/// sync and hand the machine to the kernel via sysrq (`o` poweroff,
/// `b` reboot). HALT maps to `o`: sysrq cannot halt without cutting
/// power (documented divergence). There is no graceful path for
/// SOFTREBOOT/KEXEC: the caller must NAK those.
fn graceful_shutdown(zsock: &str, sysrq_path: &str, letter: Option<u8>) -> bool {
    let list = z_list(zsock).unwrap_or_default();
    let mut ok = true;
    for (name, state) in &list {
        if z_state_tok(state) == ST_STOPPED {
            continue;
        }
        if !z_act(zsock, "stop", name) || !wait_state(zsock, name, false, 300) {
            ok = false;
        }
    }
    match letter {
        None => ok,
        Some(l) => {
            // Best effort: a failed sync must not cancel the power path.
            let _ = std::process::Command::new("sync").status();
            ok && std::fs::write(sysrq_path, [l]).is_ok()
        }
    }
}

fn cmd_compatd(path: &str, outdir: &std::path::Path) -> i32 {
    let _ = std::fs::remove_file(path);
    let listener = match UnixListener::bind(path) {
        Ok(l) => l,
        Err(e) => {
            warn(&format!("cannot bind {path}: {e}"));
            return 1;
        }
    };
    let ctx = Ctx {
        outdir: outdir.to_path_buf(),
        zsock: zinit_socket(None),
        sysrq: String::from("/proc/sysrq-trigger"),
    };
    // One thread per connection: serve_one blocks in read until the client
    // sends or disconnects, and a single-threaded loop lets one silent
    // holder wedge every later client (live: stuck add-dep parked the
    // loop, all later dinitctl hung with RC=124). Threads are std-only.
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    for conn in listener.incoming() {
        if let Ok(s) = conn {
            let counter = counter.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let mut s = s;
                let mut names = BTreeMap::new();
                serve_one(&mut s, counter, &mut names, &ctx);
            });
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

fn cmd_watch(in_dirs: &[std::path::PathBuf], out_dir: &std::path::Path) -> i32 {
    for d in in_dirs {
        if !d.is_dir() {
            warn(&format!("{}: not a directory", d.display()));
            return 1;
        }
    }
    let sock = zinit_socket(None);
    let mut last = fingerprints(in_dirs);
    // initial convert so the daemon is the source of truth from second zero.
    // One shared graph, never per-dir: a layered pass with its own partial
    // pulls disables whatever it cannot reach (etc has its own boot, so a
    // solo etc pass decimates udevd/tmpfiles/sysusers and every seasoning
    // file). That exact decimation froze an Artix cold boot to 3 services.
    if cmd_convert_dirs(in_dirs, out_dir) != 0 {
        return 1;
    }
    c4_reload(&sock);
    loop {
        std::thread::sleep(std::time::Duration::from_secs(2));
        let cur = fingerprints(in_dirs);
        if cur != last {
            last = cur;
            if cmd_convert_dirs(in_dirs, out_dir) == 0 {
                c4_reload(&sock);
            }
        }
    }
}

fn fingerprints(dirs: &[std::path::PathBuf]) -> BTreeMap<String, (u64, u64)> {
    let mut m = BTreeMap::new();
    for dir in dirs {
        for (k, v) in fingerprint(dir) {
            m.insert(format!("{}:{}", dir.display(), k), v);
        }
    }
    m
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
    // usr first so etc wins on overlay; one shared graph, so the
    // closure sees every edge no matter which layer it lives on.
    if cmd_convert_dirs(&found, &outp) != 0 {
        return 1;
    }
    // compatd service so devmon-speaking helpers keep working.
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "z-dinit-generator".to_string());
    let compat = format!(
        "type = process\ncommand = {exe} compatd --out {outdir}\nrestart = always\nrestart-budget = 3 restarts per 10s\nlog = none\nready = none\n"
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
    println!("converted {} services -> {}", converted_count(&outp), outdir);
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
            if args.len() < 2 {
                eprintln!("usage: z-dinit-generator watch <dinitdir>... <outdir>");
                2
            } else {
                let out = std::path::PathBuf::from(args.last().unwrap());
                let ins: Vec<std::path::PathBuf> =
                    args[..args.len() - 1].iter().map(std::path::PathBuf::from).collect();
                cmd_watch(&ins, &out)
            }
        }
        "dinitctl" => cmd_dinitctl(&args),
        "compatd" => {
            let mut sock = DINIT_SOCK_DEFAULT;
            let mut out = "/etc/zinit/services.d";
            let mut i = 0;
            while i < args.len() {
                if args[i] == "--out" {
                    i += 1;
                    if i >= args.len() {
                        eprintln!("usage: z-dinit-generator compatd [/run/dinitctl] [--out DIR]");
                        return std::process::ExitCode::from(2);
                    }
                    out = &args[i];
                } else if sock == DINIT_SOCK_DEFAULT {
                    sock = &args[i];
                } else {
                    eprintln!("usage: z-dinit-generator compatd [/run/dinitctl] [--out DIR]");
                    return std::process::ExitCode::from(2);
                }
                i += 1;
            }
            let rc = cmd_compatd(sock, std::path::Path::new(out));
            return std::process::ExitCode::from(rc as u8);
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
        "poweroff" | "halt" => {
            if !args.is_empty() {
                eprintln!("usage: z-dinit-generator {verb}");
                2
            } else if graceful_shutdown(&zinit_socket(None), "/proc/sysrq-trigger", Some(b'o')) {
                0
            } else {
                eprintln!("z-dinit-generator: {verb} failed");
                1
            }
        }
        "reboot" => {
            if !args.is_empty() {
                eprintln!("usage: z-dinit-generator reboot");
                2
            } else if graceful_shutdown(&zinit_socket(None), "/proc/sysrq-trigger", Some(b'b')) {
                0
            } else {
                eprintln!("z-dinit-generator: reboot failed");
                1
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
    fn dinit_secs_spelling() {
        assert_eq!(dinit_secs("5"), Some("5s".to_string()));
        assert_eq!(dinit_secs("5s"), None);
        assert_eq!(dinit_secs("0.5"), Some("500ms".to_string()));
        assert_eq!(dinit_secs("10.25"), Some("10250ms".to_string()));
        assert_eq!(dinit_secs(""), None);
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

    #[test]
    fn print_address_is_dropped_for_supervision() {
        // --print-address prints and EXITS (verified exit 0 on the host:
        // nothing left to supervise). The fd dance only existed to feed
        // it, so both go; the daemon runs foreground with no handshake.
        let dir = tmpout("printaddr");
        let mut pulls = BTreeMap::new();
        let mut befores = Vec::new();
        let mut warns = Vec::new();
        let c = convert_one(
            "type = process\ncommand = /usr/lib/dinit/dbus --print-address=4\nready-notification = pipefd:4\n",
            "dbus",
            &dir,
            &mut pulls,
            &mut befores,
            &mut warns,
        )
        .expect("converts");
        assert!(!c.text.contains("print-address"), "got:\n{}", c.text);
        assert!(!c.text.contains("exec 4>&"), "got:\n{}", c.text);
        assert!(c.text.contains("ready = none\n"), "got:\n{}", c.text);
        assert!(c.text.contains("/usr/lib/dinit/dbus"), "got:\n{}", c.text);
        assert!(warns.join("\n").contains("print-address"), "got: {warns:?}");
    }

    #[test]
    fn pipefd_dance_survives_without_print_address() {
        // A notifying daemon that does NOT print-and-exit keeps the fd
        // dance: something must write readiness to the supervisor's pipe.
        let dir = tmpout("pipedfd");
        let mut pulls = BTreeMap::new();
        let mut befores = Vec::new();
        let mut warns = Vec::new();
        let c = convert_one(
            "type = process\ncommand = /usr/lib/dbus-wait-for -s -f 4 -n org.bluez /usr/lib/bluetooth/bluetoothd\nready-notification = pipefd:4\n",
            "bluetoothd",
            &dir,
            &mut pulls,
            &mut befores,
            &mut warns,
        )
        .expect("converts");
        assert!(c.text.contains("exec 4>&$ZINIT_NOTIFY_FD"), "got:\n{}", c.text);
        assert!(c.text.contains("ready = notify\n"), "got:\n{}", c.text);
    }

    #[test]
    fn dotfiles_in_dotd_dirs_are_skipped() {
        // Real trees ship .KEEP placeholders (and symlinks) inside
        // waits-for.d/depends-on.d dirs. An after:.KEEP edge is fatal E010
        // downstream, for a file that was never a service.
        let src = tmpout("dotd-src");
        let out = tmpout("dotd-out");
        let dd = src.join("boot.d");
        std::fs::create_dir_all(&dd).unwrap();
        std::fs::write(dd.join(".KEEP"), "").unwrap();
        std::fs::write(dd.join("svc"), "type = process\ncommand = /bin/s\n").unwrap();
        std::fs::write(src.join("svc"), "type = process\ncommand = /bin/s\n").unwrap();
        std::fs::write(src.join("boot"), "type = internal\nwaits-for.d = boot.d\n").unwrap();
        assert_eq!(cmd_convert(&src, &out), 0);
        let got = std::fs::read_to_string(out.join("boot.conf")).unwrap();
        assert!(!got.contains(".KEEP"), "placeholder must not leak:\n{got}");
        assert!(got.contains("after:svc"), "real member stays:\n{got}");
    }

    #[test]
    fn layered_dirs_overlay_etc_over_usr() {
        // Later dirs win by name (dinit layering); the shadowed file
        // contributes nothing, not even its pulls.
        let usr = tmpout("layer-usr");
        let etc = tmpout("layer-etc");
        let out = tmpout("layer-out");
        std::fs::write(usr.join("a"), "type = process\ncommand = /bin/usr-a\n").unwrap();
        std::fs::write(usr.join("b"), "type = process\ncommand = /bin/usr-b\n").unwrap();
        std::fs::write(etc.join("a"), "type = process\ncommand = /bin/etc-a\n").unwrap();
        let dirs = vec![usr, etc];
        assert_eq!(cmd_convert_dirs(&dirs, &out), 0);
        let got = std::fs::read_to_string(out.join("a.conf")).unwrap();
        assert!(got.contains("command = /bin/etc-a\n"), "etc wins:\n{got}");
        assert!(std::fs::read_to_string(out.join("b.conf")).is_ok());
    }

    #[test]
    fn closure_spans_layers() {
        // The case that froze real boots: boot (etc) requires mid, mid
        // (usr) requires lib. Per-dir converts never see the whole chain
        // and disable lib; one shared graph must keep it enabled.
        let usr = tmpout("span-usr");
        let etc = tmpout("span-etc");
        let out = tmpout("span-out");
        std::fs::write(usr.join("lib"), "type = process\ncommand = /bin/lib\n").unwrap();
        std::fs::write(usr.join("mid"), "type = process\ncommand = /bin/mid\ndepends-on = lib\n")
            .unwrap();
        std::fs::write(etc.join("boot"), "type = internal\ndepends-on = mid\n").unwrap();
        let dirs = vec![usr, etc];
        assert_eq!(cmd_convert_dirs(&dirs, &out), 0);
        for svc in ["boot", "mid", "lib"] {
            let got =
                std::fs::read_to_string(out.join(format!("{svc}.conf"))).unwrap();
            assert!(
                !got.contains("enabled = no"),
                "{svc} is boot-reachable and must stay enabled:\n{got}"
            );
        }
    }

    #[test]
    fn absolute_dir_value_beats_same_named_subdir() {
        // Chimera root cause: boot (in usr) says
        // `waits-for.d: <etc>/boot.d`, but usr holds its own boot.d
        // subdir with decoys. The literal absolute path must win; the
        // file's own dir is only consulted for relative values.
        let usr = tmpout("absdir-usr");
        let etc = tmpout("absdir-etc");
        let out = tmpout("absdir-out");
        std::fs::create_dir_all(usr.join("boot.d")).unwrap();
        std::fs::write(usr.join("boot.d").join("decoy"), "type = process\ncommand = /bin/decoy\n")
            .unwrap();
        std::fs::write(usr.join("decoy"), "type = process\ncommand = /bin/decoy\n").unwrap();
        std::fs::create_dir_all(etc.join("boot.d")).unwrap();
        std::fs::write(etc.join("boot.d").join("real"), "type = process\ncommand = /bin/real\n")
            .unwrap();
        std::fs::write(etc.join("real"), "type = process\ncommand = /bin/real\n").unwrap();
        std::fs::write(
            usr.join("boot"),
            format!("type = internal\nwaits-for.d: {}/boot.d\n", etc.display()),
        )
        .unwrap();
        let dirs = vec![usr, etc];
        assert_eq!(cmd_convert_dirs(&dirs, &out), 0);
        let got = std::fs::read_to_string(out.join("real.conf")).unwrap();
        assert!(!got.contains("enabled = no"), "literal dir member must stay enabled:\n{got}");
        let got = std::fs::read_to_string(out.join("decoy.conf")).unwrap();
        assert!(got.contains("enabled = no"), "same-named subdir decoy must stay out:\n{got}");
    }

    #[test]
    fn reconvert_keeps_cross_layer_closure() {
        // The watch daemon re-converts on every change; a re-run must not
        // decimate what the first run closed (an Artix cold boot froze to
        // 3 services when the etc-only pass disabled udevd/tmpfiles and
        // every seasoning file).
        let usr = tmpout("re-usr");
        let etc = tmpout("re-etc");
        let out = tmpout("re-out");
        std::fs::write(usr.join("lib"), "type = process\ncommand = /bin/lib\n").unwrap();
        std::fs::write(usr.join("mid"), "type = process\ncommand = /bin/mid\ndepends-on = lib\n")
            .unwrap();
        std::fs::write(etc.join("boot"), "type = internal\ndepends-on = mid\n").unwrap();
        std::fs::write(etc.join("boot-extra"), "type = process\ncommand = /bin/x\n").unwrap();
        let dirs = vec![usr, etc];
        assert_eq!(cmd_convert_dirs(&dirs, &out), 0);
        assert_eq!(cmd_convert_dirs(&dirs, &out), 0);
        for svc in ["boot", "mid", "lib"] {
            let got =
                std::fs::read_to_string(out.join(format!("{svc}.conf"))).unwrap();
            assert!(
                !got.contains("enabled = no"),
                "{svc} must survive reconversion:\n{got}"
            );
        }
    }

    #[test]
    fn reconvert_single_file_uses_all_layers() {
        // The dinitctl reload/enable path re-converts on one file edit;
        // converting only that file's parent runs the closure over a
        // partial graph (an etc layer owns its own boot), decimating
        // everything outside it. Reconvert must share the graph.
        let usr = tmpout("rr-usr");
        let etc = tmpout("rr-etc");
        let out = tmpout("rr-out");
        std::fs::write(usr.join("lib"), "type = process\ncommand = /bin/lib\n").unwrap();
        std::fs::write(usr.join("mid"), "type = process\ncommand = /bin/mid\ndepends-on = lib\n")
            .unwrap();
        std::fs::write(etc.join("boot"), "type = internal\ndepends-on = mid\n").unwrap();
        let dirs = vec![usr.clone(), etc.clone()];
        assert_eq!(cmd_convert_dirs(&dirs, &out), 0);
        // Trigger from the usr layer, like `dinitctl reload mid`.
        assert!(reconvert_and_reload(&usr.join("mid"), &dirs, &out, true));
        let got = std::fs::read_to_string(out.join("lib.conf")).unwrap();
        assert!(
            !got.contains("enabled = no"),
            "cross-layer dep must survive single-file reconvert:\n{got}"
        );
    }

    #[test]
    fn operator_seasoning_survives_convert() {
        // Files convert did not generate are not its business: no
        // `enabled = no` may land on them, with or without a pin. A
        // sourceless file without a pin gets one loud warn instead.
        let src = tmpout("seas-src");
        let out = tmpout("seas-out");
        std::fs::write(src.join("boot"), "type = internal\n").unwrap();
        std::fs::write(
            out.join("net.conf"),
            "type = oneshot\ncommand = /sbin/net.sh\nrestart = never\nlog = none\nready = none\n",
        )
        .unwrap();
        std::fs::write(
            out.join("probe.conf"),
            "type = oneshot\ncommand = /bin/probe\nrestart = never\nlog = none\nready = none\nenabled = no\n",
        )
        .unwrap();
        let out2 = out.clone();
        let rc = cmd_convert(&src, &out2);
        assert_eq!(rc, 0);
        let net = std::fs::read_to_string(out.join("net.conf")).unwrap();
        assert!(
            !net.contains("enabled"),
            "operator file untouched:\n{net}"
        );
        let probe = std::fs::read_to_string(out.join("probe.conf")).unwrap();
        assert_eq!(
            probe.matches("enabled = no").count(),
            1,
            "pinned file not stacked:\n{probe}"
        );
    }

    #[test]
    fn enabled_no_is_idempotent_across_runs() {
        // `start` re-runs convert over its own output (the services dir
        // already holds enabled=no files). Stacking another copy is a
        // fatal E005 duplicate that bricks the next boot — convert must
        // be a fixed point.
        let src = tmpout("idem-src");
        let out = tmpout("idem-out");
        std::fs::write(src.join("boot"), "type = internal\n").unwrap();
        std::fs::write(src.join("lonely"), "type = process\ncommand = /bin/s\n").unwrap();
        assert_eq!(cmd_convert(&src, &out), 0);
        assert_eq!(cmd_convert(&src, &out), 0);
        let got = std::fs::read_to_string(out.join("lonely.conf")).unwrap();
        assert_eq!(
            got.lines().filter(|l| l.trim() == "enabled = no").count(),
            1,
            "exactly one enabled line after two runs:\n{got}"
        );
    }

    #[test]
    fn before_mirror_skips_when_target_already_required() {
        // elogind orders itself before logind, and logind genuinely
        // requires elogind: mirroring `after:elogind` onto logind would pair
        // it with the required edge, which zinit rejects (E010). The
        // required edge already orders the pair, so the mirror must yield.
        let src = tmpout("before-src");
        let out = tmpout("before-out");
        std::fs::write(src.join("elogind"), "type = process\ncommand = /bin/e\nbefore = logind\n")
            .unwrap();
        std::fs::write(src.join("logind"), "type = internal\ndepends-on = elogind\n").unwrap();
        assert_eq!(cmd_convert(&src, &out), 0);
        let got = std::fs::read_to_string(out.join("logind.conf")).unwrap();
        assert!(
            !got.contains("opt:elogind"),
            "soft mirror must not duplicate the required edge:\n{got}"
        );
        assert!(got.contains("elogind"), "required edge stays:\n{got}");
    }

    #[test]
    fn dep_add_refuses_ghost_targets() {
        // A dangling required edge fatals the whole boot at load, so the
        // writer must refuse ghosts (verified: an unmaterialized
        // agetty-service@ttyS0 bricked a Chimera boot). No daemon contact
        // happens on refusal paths.
        let out = tmpout("ghost");
        std::fs::write(out.join("a.conf"), "type = process\ncommand = /bin/a\n").unwrap();
        let dead = "/tmp/zdg-no-such-sock";
        assert!(!dep_add_out(&out, dead, "a", "ghost", 0));
        assert!(!dep_add_out(&out, dead, "a", "b@x", 0));
        assert!(!dep_add_out(&out, dead, "a", "../evil", 0));
        assert!(!dep_add_out(&out, dead, "../evil", "a", 0));
        assert!(!out.join("b@x.conf").exists(), "nothing materialized");
        assert!(!dep_add_out(&out, dead, "nosuch", "a", 0), "missing from-file");
    }

    #[test]
    fn dep_add_synthesizes_instance_targets() {
        // base@arg with a base template present materializes the
        // instance and lands the edge, like the find_handle path.
        use std::sync::atomic::{AtomicBool, Ordering};
        let reloaded = std::sync::Arc::new(AtomicBool::new(false));
        let (zsock, _stub) = stub(reloaded.clone());
        let out = tmpout("isynth");
        std::fs::write(out.join("a.conf"), "type = process\ncommand = /bin/a\n").unwrap();
        std::fs::write(
            out.join("base.conf"),
            "type = process\ncommand = /bin/b %i\n",
        )
        .unwrap();
        assert!(dep_add_out(&out, &zsock, "a", "base@tty1", 0));
        assert!(out.join("base@tty1.conf").is_file());
        assert!(reloaded.load(Ordering::SeqCst), "reload after synthesis");
    }

    #[test]
    fn pass_cs_fd_becomes_explicit_socket_path() {
        // dinit passes its control fd; supervised, the bridge connects
        // by path (verified: FD-less dinit-dbus exits "no file descriptor").
        let src = tmpout("cs-src");
        let mut pulls = BTreeMap::new();
        let mut befores = Vec::new();
        let mut warns = Vec::new();
        let text = "type = process\ncommand = /usr/bin/dinit-dbus -s\noptions: pass-cs-fd\n";
        let c = convert_one(&text, "svc", &src, &mut pulls, &mut befores, &mut warns)
            .expect("converts");
        assert!(
            c.text.contains("command = /usr/bin/dinit-dbus -s -S /run/dinitctl\n"),
            "socket path must be explicit:\n{}",
            c.text
        );
        assert!(warns.join("\n").contains("pass-cs-fd mapped"), "loud warn expected");
    }

    #[test]
    fn dinit_flag_is_dropped_for_supervision() {
        // dinit-patched daemons wait on PID-1 activation that never
        // comes (verified: dbus-daemon --dinit binds nothing); the
        // supervisor runs them plain so they bind themselves.
        assert_eq!(
            strip_dinit_flag("/usr/bin/dbus-daemon --system --dinit --nofork"),
            "/usr/bin/dbus-daemon --system --nofork"
        );
        assert_eq!(strip_dinit_flag("--dinit /bin/x"), "/bin/x");
        assert_eq!(strip_dinit_flag("/bin/x --dinit"), "/bin/x");
        // Longer flags survive.
        assert_eq!(strip_dinit_flag("/bin/x --dinitfoo"), "/bin/x --dinitfoo");
        let src = tmpout("dinit-src");
        let out = tmpout("dinit-out");
        std::fs::write(
            src.join("svc"),
            "type = process\ncommand = /bin/d --dinit\n",
        )
        .unwrap();
        let mut pulls = BTreeMap::new();
        let mut befores = Vec::new();
        let mut warns = Vec::new();
        let text = std::fs::read_to_string(src.join("svc")).unwrap();
        let c = convert_one(&text, "svc", &src, &mut pulls, &mut befores, &mut warns)
            .expect("converts");
        assert!(!c.text.contains("--dinit"), "flag must go:\n{}", c.text);
        assert!(c.text.contains("command = /bin/d\n"), "got:\n{}", c.text);
        assert!(
            warns.join("\n").contains("--dinit dropped"),
            "loud warn expected"
        );
        let _ = out;
    }

    #[test]
    fn after_and_waits_for_emit_order_only_edges() {
        // dinit ordering without gating: `after` and `waits-for` become
        // `after:` (the runtime waits for the first attempt, never for
        // success), while `depends-on` stays required.
        let src = tmpout("after-src");
        let out = tmpout("after-out");
        std::fs::write(
            src.join("svc"),
            "type = process\ncommand = /bin/s\ndepends-on = net\nafter = early\nwaits-for = late\n",
        )
        .unwrap();
        std::fs::write(src.join("net"), "type = process\ncommand = /bin/n\n").unwrap();
        std::fs::write(src.join("early"), "type = process\ncommand = /bin/e\n").unwrap();
        std::fs::write(src.join("late"), "type = process\ncommand = /bin/l\n").unwrap();
        assert_eq!(cmd_convert(&src, &out), 0);
        let got = std::fs::read_to_string(out.join("svc.conf")).unwrap();
        assert!(
            got.contains("depends = net, after:early, after:late\n"),
            "ordering must not gate:\n{got}"
        );
    }

    #[test]
    fn before_mirror_orders_when_no_edge_exists() {
        // Pure ordering (no dependency either way): the order-only mirror
        // is the only record of it, so it must survive.
        let src = tmpout("before-src2");
        let out = tmpout("before-out2");
        std::fs::write(src.join("early"), "type = process\ncommand = /bin/e\nbefore = late\n")
            .unwrap();
        std::fs::write(src.join("late"), "type = process\ncommand = /bin/l\n").unwrap();
        assert_eq!(cmd_convert(&src, &out), 0);
        let got = std::fs::read_to_string(out.join("late.conf")).unwrap();
        assert!(
            got.contains("depends = after:early"),
            "order-only edge must be emitted:\n{got}"
        );
    }

    // compatd protocol conformance (byte-exact vs control.cc).
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

    fn fctx() -> Ctx {
        Ctx {
            sysrq: String::from("/tmp/zdg-no-such-sysrq"),
            outdir: std::env::temp_dir().join("zdg-no-such-outdir"),
            zsock: String::from("/tmp/zdg-no-such-sock-xyz"),
        }
    }

    fn serve_bg(
        server: UnixStream,
        names: BTreeMap<u32, String>,
        ctx: Ctx,
        next: u32,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let mut server = server;
            let mut names = names;
            let counter =
                std::sync::Arc::new(std::sync::atomic::AtomicU32::new(next));
            serve_one(&mut server, counter, &mut names, &ctx);
        })
    }

    /// Stub zinit C4 server: a/b services, stop/start mutate state,
    /// reload-all is recorded. Each test gets an isolated instance.
    fn stub(
        reloaded: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> (String, std::thread::JoinHandle<()>) {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::collections::HashSet;
        let stopped = std::sync::Arc::new(std::sync::Mutex::new(HashSet::<String>::from([
            "b".to_string(),
        ])));
        static N: AtomicU32 = AtomicU32::new(100);
        let dir = std::env::temp_dir().join(format!(
            "zdg-stub-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("ctl");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let sock = path.display().to_string();
        let rl = reloaded.clone();
        let stopped = stopped.clone();
        let h = std::thread::spawn(move || {
            for conn in listener.incoming() {
                let mut c = match conn {
                    Ok(c) => BufReader::new(c),
                    Err(_) => break,
                };
                loop {
                    let mut line = String::new();
                    match c.read_line(&mut line) {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(_) => break,
                    }
                    let w: Vec<&str> = line.split_whitespace().collect();
                    let is_stopped = |n: &str| stopped.lock().unwrap().contains(n);
                    let status_of = |n: &str| -> &'static str {
                        if is_stopped(n) {
                            "R compatd 0 x stopped desired=down pid=- up=- restarts=0/3\n"
                        } else {
                            "R compatd 0 x running desired=up pid=99 up=1s restarts=0/3\n"
                        }
                    };
                    let reply = if w.len() >= 4 && w[2] == "status" {
                        match w[3] {
                            "a" | "b" => status_of(w[3]),
                            w3 if w3.contains('@') => {
                                // instances exist only once the supervisor
                                // has reloaded after synthesis.
                                let base = w3.split('@').next().unwrap_or("");
                                if (base == "a" || base == "b") && rl.load(Ordering::SeqCst) {
                                    "R compatd 0 x running desired=up pid=7 up=1s restarts=0/3\n"
                                } else {
                                    "E compatd 3 unknown service\n"
                                }
                            }
                            _ => "E compatd 3 unknown service\n",
                        }
                    } else if w.len() >= 3 && w[2] == "list" {
                        {
                            let sa = if is_stopped("a") { "stopped" } else { "running" };
                            let sb = if is_stopped("b") { "stopped" } else { "running" };
                            // string built per request; leaked to 'static for the match shape.
                            Box::leak(format!("R compatd 0 2 a={sa} b={sb}\n").into_boxed_str()) as &'static str
                        }
                    } else if w.len() >= 4 && (w[2] == "start" || w[2] == "stop" || w[2] == "restart") {
                        if w[3] == "nosuch" {
                            "E compatd 3 unknown service\n"
                        } else {
                            let mut set = stopped.lock().unwrap();
                            if w[2] == "stop" {
                                set.insert(w[3].to_string());
                            } else {
                                set.remove(w[3]);
                            }
                            "R compatd 0\n"
                        }
                    } else if w.len() >= 3 && w[2] == "reload-all" {
                        reloaded.store(true, Ordering::SeqCst);
                        "R compatd 0\n"
                    } else {
                        "E compatd 2 bad verb\n"
                    };
                    let _ = c.get_mut().write_all(reply.as_bytes());
                }
            }
        });
        (sock, h)
    }

    fn tmpout(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(1000);
        let dir = std::env::temp_dir().join(format!(
            "zdg-out-{}-{}-{}",
            tag,
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn find_handle(client: &mut UnixStream, ctx_sock_note: &str, name: &[u8]) -> u32 {
        let _ = ctx_sock_note;
        let l = name.len() as u16;
        client.write_all(&[FINDSERVICE]).unwrap();
        client.write_all(&l.to_le_bytes()).unwrap();
        client.write_all(name).unwrap();
        let r = read_exact(client, 7);
        assert_eq!(r[0], SERVICERECORD);
        u32::from_le_bytes([r[2], r[3], r[4], r[5]])
    }

    #[test]
    fn tokenizer_cooks_values() {
        assert_eq!(cook_value("a  b\tc"), "a b c");
        assert_eq!(cook_value("\"a  b\" c"), "a  b c");
        assert_eq!(cook_value("x \\y"), "x y");
        // single quotes are literal upstream: kept, never grouping.
        assert_eq!(cook_value("x 'a  b'"), "x 'a b'");
        assert_eq!(strip_comment("command = a # tail"), "command = a ");
        assert_eq!(strip_comment("command = \"a#b\""), "command = \"a#b\"");
        assert_eq!(strip_comment("# full"), "");
        let ll = logical_lines("command = a \\\n  b\nc = d\n");
        assert_eq!(ll.len(), 2);
        assert_eq!(ll[0].1, true);
        assert_eq!(cook_value(&ll[0].0), "command = a b");
        assert_eq!(ll[1].1, false);
        assert_eq!(sub_arg1("/bin/s $1 ${1} ${1:-x}", "s", &mut Vec::new()), "/bin/s %i %i %i");
    }

    #[test]
    fn convert_new_keys_end_to_end() {
        let dir = tmpout("keys-in");
        std::fs::write(dir.join("vars.env"), "FOO=bar\n# c\nBADLINE\nEMPTY=\n").unwrap();
        std::fs::write(dir.join("inc"), "waits-for: other\n").unwrap();
        let text = "type = process\ncommand = /bin/srv $1 --x=A_B # run it\ndepends-on = net\nrestart-limit-count = 5\nrestart-limit-interval = 20\nrestart-delay = 0.5\nsocket-listen = /run/s.sock\nrun-in-cgroup = /sys/fs/cgroup/sys\nrlimit-nofile = 1024:4096\nrlimit-core = 0\nlog-type = buffer\noptions: runs-on-console no-new-privs bogus-opt\nload-options: export-service-name\nenv-file = vars.env\n@include inc\n";
        let mut pulls = BTreeMap::new();
        let mut befores = Vec::new();
        let mut warns = Vec::new();
        let c = convert_one(text, "svc", &dir, &mut pulls, &mut befores, &mut warns)
            .expect("converts");
        assert!(c.text.contains("type = console\n"), "got:\n{}", c.text);
        assert!(c.text.contains("tty = /dev/console\n"), "got:\n{}", c.text);
        assert!(c.text.contains("command = /bin/srv %i --x=A_B\n"), "got:\n{}", c.text);
        assert!(c.text.contains("depends = net, after:other\n"), "got:\n{}", c.text);
        assert!(c.text.contains("restart-budget = 5 restarts per 20s\n"), "got:\n{}", c.text);
        assert!(c.text.contains("restart-delay = 500ms\n"), "got:\n{}", c.text);
        assert!(c.text.contains("listen = unix:/run/s.sock\n"), "got:\n{}", c.text);
        assert!(c.text.contains("cgroup = /sys/fs/cgroup/sys\n"), "got:\n{}", c.text);
        assert!(c.text.contains("rlimit-nofile = 1024\n"), "got:\n{}", c.text);
        assert!(c.text.contains("env = FOO=bar\n"), "got:\n{}", c.text);
        assert!(c.text.contains("env = EMPTY=\n"), "got:\n{}", c.text);
        assert!(c.text.contains("env = DINIT_SERVICE=svc\n"), "got:\n{}", c.text);
        let w = warns.join("\n");
        assert!(w.contains("log-type buffer"), "got: {w}");
        assert!(!w.contains("run-in-cgroup"), "cgroup re-emitted, no warn:\n{w}");
        assert!(w.contains("option bogus-opt ignored"), "got: {w}");
        assert!(w.contains("rlimit-core ignored"), "got: {w}");
        assert!(w.contains("env-file line ignored: BADLINE"), "got: {w}");
    }

    #[test]
    fn quoted_command_becomes_script_with_raw_text() {
        let dir = tmpout("quote-in");
        let mut pulls = BTreeMap::new();
        let mut befores = Vec::new();
        let mut warns = Vec::new();
        let c = convert_one(
            "type = process\ncommand = /bin/sh -c \"mkdir -p /run/x && exec /usr/sbin/svc -E /x.log\"\n",
            "s",
            &dir,
            &mut pulls,
            &mut befores,
            &mut warns,
        )
        .expect("converts");
        assert!(c.text.contains("type = script\n"), "got:\n{}", c.text);
        assert!(
            c.text.contains("exec /bin/sh -c \"mkdir -p /run/x && exec /usr/sbin/svc -E /x.log\"\n"),
            "got:\n{}",
            c.text
        );
        // plain commands are untouched (parity path).
        let mut warns = Vec::new();
        let c = convert_one(
            "type = process\ncommand = /bin/sleep 300\n",
            "t",
            &dir,
            &mut pulls,
            &mut befores,
            &mut warns,
        )
        .expect("converts");
        assert!(c.text.contains("type = process\n"), "got:\n{}", c.text);
        // quoted + console loses the console, loudly (only script parses).
        let mut warns = Vec::new();
        let c = convert_one(
            "type = process\ncommand = /bin/getty \"tty 1\"\noptions: runs-on-console\n",
            "g",
            &dir,
            &mut pulls,
            &mut befores,
            &mut warns,
        )
        .expect("converts");
        assert!(c.text.contains("type = script\n"), "got:\n{}", c.text);
        assert!(!c.text.contains("tty = "), "got:\n{}", c.text);
        assert!(warns.join("\n").contains("console lost"), "got: {warns:?}");
        // quoted oneshot warns loudly (no shell available there).
        let mut warns = Vec::new();
        let _ = convert_one(
            "type = scripted\ncommand = /bin/echo \"a  b\"\n",
            "o",
            &dir,
            &mut pulls,
            &mut befores,
            &mut warns,
        )
        .expect("converts");
        assert!(warns.join("\n").contains("quoted command needs a shell"), "got: {warns:?}");
    }

    #[test]
    fn convert_unlimited_budget_omits_the_line() {
        let dir = tmpout("keys0-in");
        let mut pulls = BTreeMap::new();
        let mut befores = Vec::new();
        let mut warns = Vec::new();
        let c = convert_one(
            "type = process\ncommand = /bin/a\nrestart-limit-count = 0\n",
            "s",
            &dir,
            &mut pulls,
            &mut befores,
            &mut warns,
        )
        .expect("converts");
        assert!(c.text.contains("restart = always\n"));
        assert!(!c.text.contains("restart-budget"), "got:\n{}", c.text);
    }

    #[test]
    fn instance_synthesized_from_template() {
        use std::sync::atomic::Ordering;
        let reloaded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (zsock, _stub) = stub(reloaded.clone());
        let out = tmpout("inst");
        std::fs::write(out.join("a.conf"), "type = process\ncommand = /bin/a %i\n").unwrap();
        let ctx = Ctx { outdir: out.clone(), zsock, sysrq: String::from("/tmp/zdg-no-such-sysrq") };
        let (server, mut client) = pair();
        let h = serve_bg(server, BTreeMap::new(), ctx, 96);
        let hd = find_handle(&mut client, "", b"a@tty1");
        assert_eq!(hd, 96);
        assert!(out.join("a@tty1.conf").is_file());
        assert!(reloaded.load(Ordering::SeqCst));
        drop(client);
        h.join().unwrap();
    }

    fn dctl(sock: &str, args: &[&str]) -> i32 {
        let v: Vec<String> = args.iter().map(|x| x.to_string()).collect();
        cmd_dinitctl_sock(&v, Some(sock))
    }

    #[test]
    fn dctl_signal_numbers() {
        assert_eq!(signal_number("15"), Some(15));
        assert_eq!(signal_number("TERM"), Some(15));
        assert_eq!(signal_number("term"), Some(15));
        assert_eq!(signal_number("SIGKILL"), Some(9));
        assert_eq!(signal_number("RTMIN+5"), Some(39));
        assert_eq!(signal_number("0"), None);
        assert_eq!(signal_number("65"), None);
        assert_eq!(signal_number("BOGUS"), None);
    }

    #[test]
    fn dctl_add_rm_dep_verbs() {
        use std::sync::atomic::Ordering;
        let reloaded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (zsock, _stub) = stub(reloaded.clone());
        let out = tmpout("dctl-dep");
        std::fs::write(out.join("a.conf"), "type = process\ncommand = /bin/a\n").unwrap();
        std::fs::write(out.join("b.conf"), "type = process\ncommand = /bin/b\n").unwrap();
        let od = out.display().to_string();
        // need with a running and b stopped contradicts the states -> refused.
        assert_eq!(dctl(&zsock, &["add-dep", "need", "a", "b", "--out", &od]), 1);
        // milestone is hard and acyclic here -> persists.
        assert_eq!(dctl(&zsock, &["add-dep", "milestone", "b", "a", "--out", &od]), 0);
        let text = std::fs::read_to_string(out.join("b.conf")).unwrap();
        assert!(text.contains("depends = a\n"), "got: {text}");
        // closing the loop the other way is a cycle -> refused.
        assert_eq!(dctl(&zsock, &["add-dep", "waits-for", "a", "b", "--out", &od]), 1);
        // junk type is a usage error.
        assert_eq!(dctl(&zsock, &["add-dep", "before", "a", "b", "--out", &od]), 1);
        // open the loop again, then waits-for persists.
        assert_eq!(dctl(&zsock, &["rm-dep", "milestone", "b", "a", "--out", &od]), 0);
        assert_eq!(dctl(&zsock, &["add-dep", "waits-for", "a", "b", "--out", &od]), 0);
        let text = std::fs::read_to_string(out.join("a.conf")).unwrap();
        assert!(text.contains("depends = after:b\n"), "got: {text}");
        assert!(reloaded.load(Ordering::SeqCst));
        // removal works once, then refuses.
        assert_eq!(dctl(&zsock, &["rm-dep", "waits-for", "a", "b", "--out", &od]), 0);
        assert_eq!(dctl(&zsock, &["rm-dep", "waits-for", "a", "b", "--out", &od]), 1);
    }

    #[test]
    fn dctl_enable_disable_offline() {
        let dd = tmpout("dctl-dd");
        std::fs::write(dd.join("to"), "type = process\ncommand = /bin/t\nwaits-for.d: w\n").unwrap();
        std::fs::write(dd.join("boot"), "type = internal\n").unwrap();
        let out = tmpout("dctl-dout");
        let dd_s = dd.display().to_string();
        let od = out.display().to_string();
        // default from is boot (no @meta present).
        assert_eq!(
            dctl("/tmp/zdg-no-such-sock-xyz", &["enable", "to", "--offline", "-d", &dd_s, "--out", &od]),
            0
        );
        assert!(dd.join("w").join("boot").is_symlink());
        // disabling twice: ok then refused.
        assert_eq!(
            dctl("/tmp/zdg-no-such-sock-xyz", &["disable", "to", "--offline", "-d", &dd_s, "--out", &od]),
            0
        );
        assert!(!dd.join("w").join("boot").exists());
        assert_eq!(
            dctl("/tmp/zdg-no-such-sock-xyz", &["disable", "to", "--offline", "-d", &dd_s, "--out", &od]),
            1
        );
        // unknown target refuses.
        assert_eq!(
            dctl("/tmp/zdg-no-such-sock-xyz", &["enable", "nosuch", "--offline", "-d", &dd_s, "--out", &od]),
            1
        );
        // --from selects the dependent.
        assert_eq!(
            dctl(
                "/tmp/zdg-no-such-sock-xyz",
                &["enable", "--from", "extra", "to", "--offline", "-d", &dd_s, "--out", &od]
            ),
            0
        );
        assert!(dd.join("w").join("extra").is_symlink());
    }

    #[test]
    fn dctl_enable_needs_exactly_one_waits_dir() {
        let dd = tmpout("dctl-dd2");
        std::fs::write(dd.join("to"), "type = process\ncommand = /bin/t\n").unwrap();
        let out = tmpout("dctl-dout2");
        let dd_s = dd.display().to_string();
        let od = out.display().to_string();
        assert_eq!(
            dctl("/tmp/zdg-no-such-sock-xyz", &["enable", "to", "--offline", "-d", &dd_s, "--out", &od]),
            1
        );
    }

    #[test]
    fn dctl_start_family_and_refusals() {
        // NOTE: never run these tests as root: the shutdown verb below
        // drives the real /proc/sysrq-trigger path (CI is unprivileged,
        // so it stops at EPERM and reports failure as designed).
        let reloaded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (zsock, _stub) = stub(reloaded);
        // wake/trigger collapse onto start; release onto stop.
        assert_eq!(dctl(&zsock, &["wake", "a"]), 0);
        assert_eq!(dctl(&zsock, &["trigger", "a"]), 0);
        assert_eq!(dctl(&zsock, &["release", "b"]), 0);
        assert_eq!(dctl(&zsock, &["restart", "a"]), 0);
        assert_eq!(dctl(&zsock, &["unpin", "a"]), 0);
        // stop on an unknown service with --ignore-unstarted succeeds.
        assert_eq!(dctl(&zsock, &["stop", "--ignore-unstarted", "--no-wait", "nosuch"]), 0);
        assert_eq!(dctl(&zsock, &["stop", "--no-wait", "nosuch"]), 1);
        assert_eq!(dctl(&zsock, &["signal", "--list"]), 0);
        // loud refusals.
        assert_eq!(dctl(&zsock, &["frobnicate"]), 1);
        assert_eq!(dctl(&zsock, &[]), 1);
        assert_eq!(dctl(&zsock, &["--bogus", "list"]), 1);
        // setenv stays an accepted no-op, multi-arg included.
        assert_eq!(dctl(&zsock, &["setenv", "A=1", "B"]), 0);
        assert_eq!(dctl(&zsock, &["is-started", "a"]), 0);
        assert_eq!(dctl(&zsock, &["is-started", "b"]), 1);
        assert_eq!(dctl(&zsock, &["is-failed", "b"]), 1);
        // loud refusals last: shutdown stops the stub services as a side
        // effect, so nothing stateful may follow it in this test.
        assert_eq!(dctl(&zsock, &["unload", "a"]), 1);
        assert_eq!(dctl(&zsock, &["shutdown"]), 1);
        assert_eq!(dctl(&zsock, &["untrigger", "a"]), 1);
    }

    #[test]
    fn dctl_catlog_and_clear() {
        let reloaded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (zsock, _stub) = stub(reloaded);
        let out = tmpout("dctl-cat");
        let logp = out.join("s.log");
        std::fs::write(&logp, "logged\n").unwrap();
        std::fs::write(
            out.join("a.conf"),
            format!("type = process\ncommand = /bin/a\nlog = file:{}\n", logp.display()),
        )
        .unwrap();
        let od = out.display().to_string();
        assert_eq!(dctl(&zsock, &["catlog", "a", "--out", &od]), 0);
        assert_eq!(dctl(&zsock, &["catlog", "--clear", "a", "--out", &od]), 0);
        assert_eq!(std::fs::read_to_string(&logp).unwrap(), "");
        assert_eq!(dctl(&zsock, &["catlog", "b", "--out", &od]), 1);
    }

    #[test]
    fn dctl_reload_offline_reconverts() {
        let dd = tmpout("dctl-rl");
        std::fs::write(dd.join("a"), "type = process\ncommand = /bin/a\n").unwrap();
        let out = tmpout("dctl-rlout");
        let dd_s = dd.display().to_string();
        let od = out.display().to_string();
        assert_eq!(
            dctl("/tmp/zdg-no-such-sock-xyz", &["reload", "a", "--offline", "-d", &dd_s, "--out", &od]),
            0
        );
        assert!(out.join("a.conf").is_file());
        assert_eq!(
            dctl("/tmp/zdg-no-such-sock-xyz", &["reload", "nosuch", "--offline", "-d", &dd_s, "--out", &od]),
            1
        );
    }

    #[test]
    fn graceful_shutdown_remain_stops_without_sysrq() {
        let reloaded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (zsock, _stub) = stub(reloaded);
        let sysrq = tmpout("remain").join("sysrq");
        assert!(graceful_shutdown(&zsock, &sysrq.display().to_string(), None));
        // a was running: stopped now; the trigger file stays untouched.
        assert!(z_status(&zsock, "a").map(|st| st.state == ST_STOPPED).unwrap_or(false));
        assert!(!sysrq.exists());
    }

    #[test]
    fn graceful_shutdown_poweroff_writes_the_letter() {
        let reloaded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (zsock, _stub) = stub(reloaded);
        let sysrq = tmpout("off").join("sysrq");
        let path = sysrq.display().to_string();
        assert!(graceful_shutdown(&zsock, &path, Some(b'o')));
        assert_eq!(std::fs::read(&sysrq).unwrap(), b"o");
    }

    #[test]
    fn compatd_shutdown_remain_acks_and_softreboot_naks() {
        let reloaded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (zsock, _stub) = stub(reloaded);
        let out = tmpout("shut");
        let sysrq = tmpout("shut-sys").join("sysrq");
        let ctx = Ctx {
            outdir: out,
            zsock,
            sysrq: sysrq.display().to_string(),
        };
        let (server, mut client) = pair();
        let h = serve_bg(server, BTreeMap::new(), ctx, 97);
        let ah = find_handle(&mut client, "", b"a");
        client.write_all(&[SHUTDOWN, 1]).unwrap();
        assert_eq!(read_exact(&mut client, 1), [ACK]);
        // a is stopped now (fresh handle: service re-queried live).
        let ah2 = find_handle(&mut client, "", b"a");
        assert_ne!(ah, ah2);
        client.write_all(&[SERVICESTATUSQ]).unwrap();
        client.write_all(&ah2.to_le_bytes()).unwrap();
        let st = read_exact(&mut client, 12);
        assert_eq!(st[2], ST_STOPPED);
        client.write_all(&[SHUTDOWN, 5]).unwrap();
        assert_eq!(read_exact(&mut client, 1), [NAK]);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn version_handshake() {
        let (server, mut client) = pair();
        let h = serve_bg(server, BTreeMap::new(), fctx(), 0);
        client.write_all(&[QUERYVERSION]).unwrap();
        assert_eq!(read_exact(&mut client, 5), VER_REPLY);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn find_canned_without_supervisor() {
        let (server, mut client) = pair();
        let h = serve_bg(server, BTreeMap::new(), fctx(), 41);
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
    fn find_rejects_dot_name() {
        let (server, mut client) = pair();
        let h = serve_bg(server, BTreeMap::new(), fctx(), 0);
        client.write_all(&[FINDSERVICE, 2, 0]).unwrap();
        client.write_all(b".x").unwrap();
        assert_eq!(read_exact(&mut client, 1), [BADREQ]);
        assert_eq!(read_exact(&mut client, 1), []);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn close_unknown_gets_badreq_and_close() {
        let (server, mut client) = pair();
        let h = serve_bg(server, BTreeMap::new(), fctx(), 0);
        client.write_all(&[CLOSEHANDLE, 9, 9, 9, 9]).unwrap();
        assert_eq!(read_exact(&mut client, 1), [BADREQ]);
        assert_eq!(read_exact(&mut client, 1), []);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn close_known_acks_then_forgets() {
        let (server, mut client) = pair();
        let h = serve_bg(server, BTreeMap::new(), fctx(), 41);
        let hd = find_handle(&mut client, "", b"system");
        assert_eq!(hd, 41);
        client.write_all(&[CLOSEHANDLE]).unwrap();
        client.write_all(&hd.to_le_bytes()).unwrap();
        assert_eq!(read_exact(&mut client, 1), [ACK]);
        client.write_all(&[CLOSEHANDLE]).unwrap();
        client.write_all(&hd.to_le_bytes()).unwrap();
        assert_eq!(read_exact(&mut client, 1), [BADREQ]);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn silent_holder_does_not_wedge_next_client() {
        // Live 2026-10-16: a stuck `dinitctl add-dep` parked serve_one in
        // read forever and every later v7 client hung (RC=124). One thread
        // per connection: the silent holder keeps its thread, the next
        // client still gets answers. pair() clients time out after 5 s, so
        // the old sequential loop fails this test instead of hanging CI.
        let (s1, c1) = pair();
        let (s2, mut c2) = pair();
        let h1 = serve_bg(s1, BTreeMap::new(), fctx(), 0);
        let h2 = serve_bg(s2, BTreeMap::new(), fctx(), 1000);
        // c1 holds silently: connected, never writes, never EOFs.
        c2.write_all(&[QUERYVERSION]).unwrap();
        assert_eq!(read_exact(&mut c2, 5), VER_REPLY);
        let name = b"zzz-no-such-svc";
        let mut q = vec![FINDSERVICE, name.len() as u8, 0];
        q.extend_from_slice(name);
        c2.write_all(&q).unwrap();
        // No sup at the fixture socket: canned STARTED record, first byte
        // SERVICERECORD either way (never a hang, never NOSERVICE here).
        let reply = read_exact(&mut c2, 1);
        assert_eq!(reply, vec![SERVICERECORD]);
        drop(c1);
        drop(c2);
        h1.join().unwrap();
        h2.join().unwrap();
    }

    #[test]
    fn unpin_acks_without_pin_store() {
        let (server, mut client) = pair();
        let mut names = BTreeMap::new();
        names.insert(7, "x".to_string());
        let h = serve_bg(server, names, fctx(), 8);
        client.write_all(&[UNPINSERVICE, 7, 0, 0, 0]).unwrap();
        assert_eq!(read_exact(&mut client, 1), [ACK]);
        client.write_all(&[UNPINSERVICE, 8, 0, 0, 0]).unwrap();
        assert_eq!(read_exact(&mut client, 1), [BADREQ]);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn queryname_roundtrip() {
        let (server, mut client) = pair();
        let h = serve_bg(server, BTreeMap::new(), fctx(), 5);
        let hd = find_handle(&mut client, "", b"dbus");
        client.write_all(&[QUERYSERVICENAME, 0]).unwrap();
        client.write_all(&hd.to_le_bytes()).unwrap();
        let hdr = read_exact(&mut client, 4);
        assert_eq!(&hdr[..2], &[SERVICENAME, 0]);
        let n = u16::from_le_bytes([hdr[2], hdr[3]]) as usize;
        assert_eq!(read_exact(&mut client, n), b"dbus");
        client.write_all(&[QUERYSERVICENAME, 0, 9, 9, 9, 9]).unwrap();
        assert_eq!(read_exact(&mut client, 1), [NAK]);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn setenv_accepts_assignment_rejects_bare() {
        let (server, mut client) = pair();
        let h = serve_bg(server, BTreeMap::new(), fctx(), 0);
        client.write_all(&[SETENV, 3, 0]).unwrap();
        client.write_all(b"a=b").unwrap();
        assert_eq!(read_exact(&mut client, 1), [ACK]);
        client.write_all(&[SETENV, 2, 0]).unwrap();
        client.write_all(b"=x").unwrap();
        assert_eq!(read_exact(&mut client, 1), [BADREQ]);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn getallenv_empty_and_reserved_rejected() {
        let (server, mut client) = pair();
        let h = serve_bg(server, BTreeMap::new(), fctx(), 0);
        client.write_all(&[GETALLENV, 0]).unwrap();
        let r = read_exact(&mut client, 1 + std::mem::size_of::<usize>());
        assert_eq!(r[0], ALLENV);
        assert!(r[1..].iter().all(|&b| b == 0));
        client.write_all(&[GETALLENV, 1]).unwrap();
        assert_eq!(read_exact(&mut client, 1), [BADREQ]);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn listenenv_acks() {
        let (server, mut client) = pair();
        let h = serve_bg(server, BTreeMap::new(), fctx(), 0);
        client.write_all(&[LISTENENV]).unwrap();
        assert_eq!(read_exact(&mut client, 1), [ACK]);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn shutdown_types_map_to_graceful_or_nak() {
        let (server, mut client) = pair();
        let h = serve_bg(server, BTreeMap::new(), fctx(), 0);
        // With no supervisor the list is empty: graceful trivially holds,
        // and the sysrq file lands in /tmp (never the real trigger here).
        client.write_all(&[SHUTDOWN, 4]).unwrap();
        assert_eq!(read_exact(&mut client, 1), [ACK]);
        client.write_all(&[SHUTDOWN, 5]).unwrap();
        assert_eq!(read_exact(&mut client, 1), [NAK]);
        client.write_all(&[SHUTDOWN, 9]).unwrap();
        assert_eq!(read_exact(&mut client, 1), [BADREQ]);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn unload_naks_loaded_badreqs_unknown() {
        let (server, mut client) = pair();
        let mut names = BTreeMap::new();
        names.insert(3, "s".to_string());
        let h = serve_bg(server, names, fctx(), 4);
        client.write_all(&[UNLOADSERVICE, 3, 0, 0, 0]).unwrap();
        assert_eq!(read_exact(&mut client, 1), [NAK]);
        client.write_all(&[UNLOADSERVICE, 77, 0, 0, 0]).unwrap();
        assert_eq!(read_exact(&mut client, 1), [BADREQ]);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn trigger_always_naks() {
        let (server, mut client) = pair();
        let mut names = BTreeMap::new();
        names.insert(3, "s".to_string());
        let h = serve_bg(server, names, fctx(), 4);
        client.write_all(&[SETTRIGGER, 3, 0, 0, 0, 1]).unwrap();
        assert_eq!(read_exact(&mut client, 1), [NAK]);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn unknown_type_gets_badreq_and_close() {
        let (server, mut client) = pair();
        let h = serve_bg(server, BTreeMap::new(), fctx(), 0);
        client.write_all(&[200]).unwrap();
        assert_eq!(read_exact(&mut client, 1), [BADREQ]);
        assert_eq!(read_exact(&mut client, 1), []);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn find_reports_live_state_and_noservice() {
        let reloaded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (zsock, _stub) = stub(reloaded);
        let out = tmpout("find");
        let ctx = Ctx { outdir: out, zsock, sysrq: String::from("/tmp/zdg-no-such-sysrq") };
        let (server, mut client) = pair();
        let h = serve_bg(server, BTreeMap::new(), ctx, 10);
        let hd = find_handle(&mut client, "", b"a");
        assert_eq!(hd, 10);
        client.write_all(&[FINDSERVICE, 5, 0]).unwrap();
        client.write_all(b"nope!").unwrap();
        assert_eq!(read_exact(&mut client, 1), [NOSERVICE]);
        let hd2 = find_handle(&mut client, "", b"b");
        assert_eq!(hd2, 11);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn start_reaches_alreadyss_with_events_and_preack() {
        let reloaded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (zsock, _stub) = stub(reloaded);
        let out = tmpout("start");
        let ctx = Ctx { outdir: out, zsock, sysrq: String::from("/tmp/zdg-no-such-sysrq") };
        let (server, mut client) = pair();
        let h = serve_bg(server, BTreeMap::new(), ctx, 20);
        let hd = find_handle(&mut client, "", b"a");
        client.write_all(&[STARTSERVICE, 128]).unwrap();
        client.write_all(&hd.to_le_bytes()).unwrap();
        assert_eq!(read_exact(&mut client, 1), [PREACK]);
        let e1 = read_exact(&mut client, 17);
        assert_eq!(&e1[..2], &[SERVICEEVENT, 17]);
        assert_eq!(&e1[2..6], &hd.to_le_bytes());
        assert_eq!(e1[6], 0);
        let e5 = read_exact(&mut client, 21);
        assert_eq!(&e5[..2], &[SERVICEEVENT5, 21]);
        assert_eq!(&e5[2..6], &hd.to_le_bytes());
        assert_eq!(e5[6], 0);
        assert_eq!(read_exact(&mut client, 1), [ALREADYSS]);
        client.write_all(&[STARTSERVICE, 0, 99, 0, 0, 0]).unwrap();
        assert_eq!(read_exact(&mut client, 1), [BADREQ]);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn stop_lands_alreadyss_on_stopped() {
        let reloaded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (zsock, _stub) = stub(reloaded);
        let out = tmpout("stop");
        let ctx = Ctx { outdir: out, zsock, sysrq: String::from("/tmp/zdg-no-such-sysrq") };
        let (server, mut client) = pair();
        let h = serve_bg(server, BTreeMap::new(), ctx, 30);
        let hd = find_handle(&mut client, "", b"b");
        client.write_all(&[STOPSERVICE, 0]).unwrap();
        client.write_all(&hd.to_le_bytes()).unwrap();
        let e1 = read_exact(&mut client, 17);
        assert_eq!(e1[6], 1);
        let _ = read_exact(&mut client, 21);
        assert_eq!(read_exact(&mut client, 1), [ALREADYSS]);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn status_buffers_match_control_layout() {
        let reloaded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (zsock, _stub) = stub(reloaded);
        let out = tmpout("status");
        std::fs::write(out.join("a.conf"), "type = process\ncommand = /bin/a\n").unwrap();
        let ctx = Ctx { outdir: out, zsock, sysrq: String::from("/tmp/zdg-no-such-sysrq") };
        let (server, mut client) = pair();
        let h = serve_bg(server, BTreeMap::new(), ctx, 40);
        let hd = find_handle(&mut client, "", b"a");
        client.write_all(&[SERVICESTATUSQ]).unwrap();
        client.write_all(&hd.to_le_bytes()).unwrap();
        let r = read_exact(&mut client, 12);
        assert_eq!(&r[..2], &[SERVICESTATUSR, 0]);
        assert_eq!(r[2], ST_STARTED);
        assert_eq!(&r[8..12], &99i32.to_le_bytes());
        client.write_all(&[SERVICESTATUS6Q]).unwrap();
        client.write_all(&hd.to_le_bytes()).unwrap();
        let r6 = read_exact(&mut client, 32);
        assert_eq!(&r6[..2], &[SERVICESTATUSR, 0]);
        client.write_all(&[SERVICESTATUSQ, 77, 0, 0, 0]).unwrap();
        assert_eq!(read_exact(&mut client, 1), [NAK]);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn list5_reports_every_service_then_done() {
        let reloaded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (zsock, _stub) = stub(reloaded);
        let out = tmpout("list");
        let ctx = Ctx { outdir: out, zsock, sysrq: String::from("/tmp/zdg-no-such-sysrq") };
        let (server, mut client) = pair();
        let h = serve_bg(server, BTreeMap::new(), ctx, 50);
        client.write_all(&[LISTSERVICES5]).unwrap();
        let r = read_exact(&mut client, 2 * 17 + 1);
        assert_eq!(r[0], SVCINFO);
        assert_eq!(r[17], SVCINFO);
        assert_eq!(r[34], LISTDONE);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn add_then_remove_dep_persists_and_reloads() {
        use std::sync::atomic::Ordering;
        let reloaded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (zsock, _stub) = stub(reloaded.clone());
        let out = tmpout("dep");
        std::fs::write(out.join("a.conf"), "type = process\ncommand = /bin/a\n").unwrap();
        std::fs::write(out.join("b.conf"), "type = process\ncommand = /bin/b\n").unwrap();
        let ctx = Ctx { outdir: out.clone(), zsock, sysrq: String::from("/tmp/zdg-no-such-sysrq") };
        let (server, mut client) = pair();
        let h = serve_bg(server, BTreeMap::new(), ctx, 60);
        // stub services a (running) and b (stopped); REGULAR add with
        // from running and to stopped contradicts dinit rules → NAK.
        let fh = find_handle(&mut client, "", b"a");
        let th = find_handle(&mut client, "", b"b");
        let mut pkt = vec![ADD_DEP, 0];
        pkt.extend_from_slice(&fh.to_le_bytes());
        pkt.extend_from_slice(&th.to_le_bytes());
        client.write_all(&pkt).unwrap();
        assert_eq!(read_exact(&mut client, 1), [NAK]);
        // WAITS_FOR has no state rule: persists `after:b` + reload-all.
        let mut pkt = vec![ADD_DEP, 2];
        pkt.extend_from_slice(&fh.to_le_bytes());
        pkt.extend_from_slice(&th.to_le_bytes());
        client.write_all(&pkt).unwrap();
        assert_eq!(read_exact(&mut client, 1), [ACK]);
        let text = std::fs::read_to_string(out.join("a.conf")).unwrap();
        assert!(text.contains("depends = after:b\n"), "got: {text}");
        assert!(reloaded.load(Ordering::SeqCst));
        // duplicate add is a quiet ACK.
        client.write_all(&pkt).unwrap();
        assert_eq!(read_exact(&mut client, 1), [ACK]);
        // removal returns to no-depends + ACK.
        let mut rp = vec![REM_DEP, 2];
        rp.extend_from_slice(&fh.to_le_bytes());
        rp.extend_from_slice(&th.to_le_bytes());
        client.write_all(&rp).unwrap();
        assert_eq!(read_exact(&mut client, 1), [ACK]);
        let text = std::fs::read_to_string(out.join("a.conf")).unwrap();
        assert!(!text.contains("depends"), "got: {text}");
        // removing again is NAK.
        client.write_all(&rp).unwrap();
        assert_eq!(read_exact(&mut client, 1), [NAK]);
        // cycle: b already order-waited on a, so a -> b is refused.
        std::fs::write(out.join("b.conf"), "type = process\ncommand = /bin/b\ndepends = after:a\n").unwrap();
        client.write_all(&pkt).unwrap();
        assert_eq!(read_exact(&mut client, 1), [NAK]);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn signal_paths_report_like_control() {
        let reloaded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (zsock, _stub) = stub(reloaded);
        let out = tmpout("sig");
        let ctx = Ctx { outdir: out, zsock, sysrq: String::from("/tmp/zdg-no-such-sysrq") };
        let (server, mut client) = pair();
        let h = serve_bg(server, BTreeMap::new(), ctx, 80);
        let bh = find_handle(&mut client, "", b"b");
        // stopped service has no pid.
        let mut p = vec![SIGNALQ];
        p.extend_from_slice(&15i32.to_le_bytes());
        p.extend_from_slice(&bh.to_le_bytes());
        client.write_all(&p).unwrap();
        assert_eq!(read_exact(&mut client, 1), [SIGNAL_NOPID]);
        // junk signal number.
        let ah = find_handle(&mut client, "", b"a");
        let mut p = vec![SIGNALQ];
        p.extend_from_slice(&0i32.to_le_bytes());
        p.extend_from_slice(&ah.to_le_bytes());
        client.write_all(&p).unwrap();
        assert_eq!(read_exact(&mut client, 1), [SIGNAL_BADSIG]);
        // unknown handle → NAK, connection stays open.
        let mut p = vec![SIGNALQ];
        p.extend_from_slice(&15i32.to_le_bytes());
        p.extend_from_slice(&77u32.to_le_bytes());
        client.write_all(&p).unwrap();
        assert_eq!(read_exact(&mut client, 1), [NAK]);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn catlog_serves_file_logs_naks_the_rest() {
        let reloaded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (zsock, _stub) = stub(reloaded);
        let out = tmpout("catlog");
        let logp = out.join("svc.log");
        std::fs::write(&logp, "hello-log").unwrap();
        std::fs::write(
            out.join("a.conf"),
            format!("type = process\ncommand = /bin/a\nlog = file:{}\n", logp.display()),
        )
        .unwrap();
        std::fs::write(out.join("b.conf"), "type = process\ncommand = /bin/b\nlog = none\n").unwrap();
        let ctx = Ctx { outdir: out, zsock, sysrq: String::from("/tmp/zdg-no-such-sysrq") };
        let (server, mut client) = pair();
        let h = serve_bg(server, BTreeMap::new(), ctx, 90);
        let ah = find_handle(&mut client, "", b"a");
        client.write_all(&[CATLOG, 0]).unwrap();
        client.write_all(&ah.to_le_bytes()).unwrap();
        let hdr = read_exact(&mut client, 3);
        assert_eq!(&hdr[..2], &[SERVICE_LOG, 0]);
        assert_eq!(read_exact(&mut client, hdr[2] as usize), b"hello-log");
        let bh = find_handle(&mut client, "", b"b");
        client.write_all(&[CATLOG, 0]).unwrap();
        client.write_all(&bh.to_le_bytes()).unwrap();
        assert_eq!(read_exact(&mut client, 1), [NAK]);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn dscdir_and_loadmech_describe_the_store() {
        let reloaded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (zsock, _stub) = stub(reloaded);
        let out = tmpout("dsc");
        let ctx = Ctx {
            outdir: out.clone(),
            zsock,
            sysrq: String::from("/tmp/zdg-no-such-sysrq"),
        };
        let (server, mut client) = pair();
        let h = serve_bg(server, BTreeMap::new(), ctx, 95);
        let ah = find_handle(&mut client, "", b"a");
        client.write_all(&[QUERYSERVICEDSCDIR, 0]).unwrap();
        client.write_all(&ah.to_le_bytes()).unwrap();
        let hdr = read_exact(&mut client, 5);
        assert_eq!(hdr[0], SVCDSCDIR);
        let n = u32::from_le_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]) as usize;
        let got = read_exact(&mut client, n);
        assert_eq!(got, out.display().to_string().as_bytes());
        client.write_all(&[QUERY_LOAD_MECH]).unwrap();
        let lm = read_exact(&mut client, 2);
        assert_eq!(lm, [LOADER_MECH, 1]);
        drop(client);
        h.join().unwrap();
    }
}
