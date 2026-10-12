// dinit-compatd — minimal dinit control-protocol server for devmon.
//
// Speaks just enough of the daemon side for Chimera's devmon helper:
// QUERYVERSION, FINDSERVICE/LOADSERVICE (canned STARTED + fresh handle),
// dependency add/remove/enable, wake and close-handle (all ACK).
// Steady state is silent: devmon idles in poll with zero traffic
// (verified: 0 syscalls in 10 s against real dinit), so there is no
// event engine here on purpose. Anything else closes the connection,
// like BADREQ would, minus one round trip.
//
// No dependencies, no Cargo project: `rustc -O --edition=2021 compatd.rs`.
// Usage: compatd [/run/dinitctl]. Exit nonzero only when it cannot bind.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};

const QUERYVERSION: u8 = 0;
const FINDSERVICE: u8 = 1;
const LOADSERVICE: u8 = 2;
const WAKESERVICE: u8 = 5;
const CLOSEHANDLE: u8 = 23;
const CPVERSION: u8 = 58;
const SERVICERECORD: u8 = 59;
const ACK: u8 = 50;
const BADREQ: u8 = 52;
// Protocol 7, compatible back to 1: what real dinit answers.
const VER_REPLY: [u8; 5] = [CPVERSION, 1, 0, 7, 0];

fn read_n(s: &mut UnixStream, mut buf: &mut [u8]) -> bool {
    while !buf.is_empty() {
        match s.read(buf) {
            Ok(0) => return false,
            Ok(n) => {
                buf = &mut buf[n..];
            }
            Err(_) => return false,
        }
    }
    true
}

fn write_all(s: &mut UnixStream, mut buf: &[u8]) -> bool {
    while !buf.is_empty() {
        match s.write(buf) {
            Ok(0) => return false,
            Ok(n) => buf = &buf[n..],
            Err(_) => return false,
        }
    }
    true
}

fn read_u16(s: &mut UnixStream) -> Option<u16> {
    let mut b = [0u8; 2];
    read_n(s, &mut b).then(|| u16::from_le_bytes(b))
}

// Serve one client until EOF, error or protocol violation. Returns the
// next handle value so handles stay unique across the process lifetime.
fn serve(s: &mut UnixStream, mut next_handle: u32) -> u32 {
    loop {
        let mut t = [0u8; 1];
        if !read_n(s, &mut t) {
            return next_handle;
        }
        match t[0] {
            QUERYVERSION => {
                if !write_all(s, &VER_REPLY) {
                    return next_handle;
                }
            }
            FINDSERVICE | LOADSERVICE => {
                let (Some(len), mut name) = (read_u16(s), Vec::new()) else {
                    return next_handle;
                };
                if len == 0 || len > 1021 {
                    let _ = write_all(s, &[BADREQ]);
                    return next_handle;
                }
                name.resize(len as usize, 0);
                if !read_n(s, &mut name) {
                    return next_handle;
                }
                // STARTED/STARTED + fresh handle, like a settled system.
                let mut reply = [SERVICERECORD, 2, 0, 0, 0, 0, 2];
                reply[2..6].copy_from_slice(&next_handle.to_le_bytes());
                next_handle += 1;
                if !write_all(s, &reply) {
                    return next_handle;
                }
            }
            // Dep add/remove/enable: cmd + dtype + from-handle + to-handle.
            11 | 12 | 14 => {
                let mut rest = [0u8; 9];
                if !read_n(s, &mut rest) || !write_all(s, &[ACK]) {
                    return next_handle;
                }
            }
            // Wake: flags + handle.
            WAKESERVICE => {
                let mut rest = [0u8; 5];
                if !read_n(s, &mut rest) || !write_all(s, &[ACK]) {
                    return next_handle;
                }
            }
            // Close handle.
            CLOSEHANDLE => {
                let mut rest = [0u8; 4];
                if !read_n(s, &mut rest) || !write_all(s, &[ACK]) {
                    return next_handle;
                }
            }
            _ => {
                let _ = write_all(s, &[BADREQ]);
                return next_handle;
            }
        }
    }
}

fn main() {
    let path = std::env::args().nth(1).unwrap_or("/run/dinitctl".into());
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("compatd: cannot bind {path}: {e}");
            std::process::exit(1);
        }
    };
    let mut next_handle = 0u32;
    for conn in listener.incoming() {
        if let Ok(mut s) = conn {
            next_handle = serve(&mut s, next_handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn pair() -> (UnixStream, UnixStream) {
        let dir = std::env::temp_dir().join(format!("compatd-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("ctl");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let mut client = UnixStream::connect(&path).unwrap();
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
        let h = std::thread::spawn(move || serve(&mut server, 0));
        client.write_all(&[QUERYVERSION]).unwrap();
        assert_eq!(read_exact(&mut client, 5), VER_REPLY);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn find_returns_started_with_fresh_handles() {
        let (mut server, mut client) = pair();
        let h = std::thread::spawn(move || serve(&mut server, 41));
        client.write_all(&[FINDSERVICE, 6, 0]).unwrap();
        client.write_all(b"system").unwrap();
        let r = read_exact(&mut client, 7);
        assert_eq!(&r[..2], &[SERVICERECORD, 2]);
        assert_eq!(&r[2..6], &41u32.to_le_bytes());
        assert_eq!(r[6], 2);
        client.write_all(&[LOADSERVICE, 1, 0]).unwrap();
        client.write_all(b"x").unwrap();
        let r2 = read_exact(&mut client, 7);
        assert_eq!(&r2[2..6], &42u32.to_le_bytes());
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn mutating_verbs_ack() {
        let (mut server, mut client) = pair();
        let h = std::thread::spawn(move || serve(&mut server, 0));
        // dep add: cmd + dtype + 2 handles.
        client.write_all(&[11, 2, 0, 0, 0, 0, 1, 0, 0, 0]).unwrap();
        assert_eq!(read_exact(&mut client, 1), [ACK]);
        // wake: flags + handle.
        client.write_all(&[WAKESERVICE, 0, 7, 0, 0, 0]).unwrap();
        assert_eq!(read_exact(&mut client, 1), [ACK]);
        // close: handle.
        client.write_all(&[CLOSEHANDLE, 7, 0, 0, 0]).unwrap();
        assert_eq!(read_exact(&mut client, 1), [ACK]);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn unknown_type_gets_badreq_and_close() {
        let (mut server, mut client) = pair();
        let h = std::thread::spawn(move || serve(&mut server, 0));
        client.write_all(&[200]).unwrap();
        assert_eq!(read_exact(&mut client, 1), [BADREQ]);
        assert_eq!(read_exact(&mut client, 1), []);
        drop(client);
        h.join().unwrap();
    }
}
