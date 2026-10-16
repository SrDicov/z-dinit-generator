#!/usr/bin/env python3
# test-proto.py — raw-protocol conformance of the real compatd binary.
# Speaks dinit v7 bytes against `z-dinit-generator compatd` backed by a
# stub zinit C4 server. Fails loud on the first mismatch; silent on green.
# Usage: test-proto.py <zdg-binary>
import os
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time

BIN = sys.argv[1]
TMP = tempfile.mkdtemp(prefix="proto-")
OUT = os.path.join(TMP, "out")
os.mkdir(OUT)
LOGF = os.path.join(TMP, "s.log")
open(LOGF, "w").write("logged")
with open(os.path.join(OUT, "a.conf"), "w") as f:
    f.write("type = process\ncommand = /bin/a\nlog = file:%s\n" % LOGF)
with open(os.path.join(OUT, "b.conf"), "w") as f:
    f.write("type = process\ncommand = /bin/b\n")

STUB = os.path.join(TMP, "c4.sock")
DSOCK = os.path.join(TMP, "dinitctl")
reloaded = []
# NOTE: power verbs go through the stub supervisor (C4), never sysrq, so
# this e2e is root-safe: the worst a power verb does here is an ACK.
stopped = {"b"}
lag_used = [False]


def stub_server():
    if os.path.exists(STUB):
        os.remove(STUB)
    ls = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    ls.bind(STUB)
    ls.listen(8)
    ls.settimeout(30)
    while True:
        try:
            c, _ = ls.accept()
        except socket.timeout:
            return
        c.settimeout(10)
        buf = b""
        try:
            while True:
                line_end = buf.find(b"\n")
                if line_end < 0:
                    chunk = c.recv(4096)
                    if not chunk:
                        break
                    buf += chunk
                    continue
                line = buf[:line_end].decode()
                buf = buf[line_end + 1:]
                w = line.split()
                if len(w) >= 4 and w[2] == "status":
                    name = w[3]
                    base = name.split("@")[0] if "@" in name else name
                    if base not in ("a", "b"):
                        rep = "E compatd 3 unknown service\n"
                    elif "@" in name and not reloaded:
                        # instances exist only once the supervisor has
                        # reloaded after synthesis.
                        rep = "E compatd 3 unknown service\n"
                    elif name in stopped or base in stopped:
                        rep = "R compatd 0 x stopped desired=down pid=- up=- restarts=0/3\n"
                    else:
                        rep = "R compatd 0 x running desired=up pid=99 up=1s restarts=0/3\n"
                elif len(w) >= 3 and w[2] == "list":
                    sa = "stopped" if "a" in stopped else "running"
                    sb = "stopped" if "b" in stopped else "running"
                    rep = "R compatd 0 2 a=%s b=%s\n" % (sa, sb)
                elif len(w) >= 4 and w[2] in ("start", "stop", "restart"):
                    if w[3] not in ("a", "b"):
                        rep = "E compatd 3 unknown service\n"
                    else:
                        # lag simulation, one-shot: the first `stop a` is
                        # accepted but never lands (exercises the ACK path);
                        # later stops apply, so SHUTDOWN can stop the world.
                        if w[2] == "stop" and w[3] == "a" and not lag_used[0]:
                            lag_used[0] = True
                        elif w[2] == "stop":
                            stopped.add(w[3])
                        else:
                            stopped.discard(w[3])
                        rep = "R compatd 0\n"
                elif len(w) >= 3 and w[2] == "reload-all":
                    reloaded.append(1)
                    rep = "R compatd 0\n"
                elif len(w) >= 3 and w[2] in ("poweroff", "reboot", "halt", "suspend"):
                    rep = "R compatd 0\n"
                else:
                    rep = "E compatd 2 bad\n"
                c.sendall(rep.encode())
        except (ConnectionResetError, BrokenPipeError, socket.timeout):
            pass
        finally:
            c.close()


threading.Thread(target=stub_server, daemon=True).start()
env = dict(os.environ, ZINIT_SOCKET=STUB)
proc = subprocess.Popen([BIN, "compatd", DSOCK, "--out", OUT], env=env,
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
try:
    for _ in range(100):
        if os.path.exists(DSOCK):
            break
        time.sleep(0.1)
    else:
        print("FAIL: compatd did not bind")
        sys.exit(1)

    class C:
        def __init__(self):
            self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            self.s.settimeout(10)
            self.s.connect(DSOCK)

        def raw(self, data):
            self.s.sendall(data)

        def read(self, n):
            out = b""
            while len(out) < n:
                chunk = self.s.recv(n - len(out))
                if not chunk:
                    break
                out += chunk
            return out

        def close(self):
            self.s.close()

    fails = []

    def check(name, cond, extra=""):
        if not cond:
            fails.append(name)
            print("FAIL: %s %s" % (name, extra))

    def find(c, name):
        nb = name.encode()
        c.raw(bytes([1]) + struct.pack("<H", len(nb)) + nb)
        r = c.read(7)
        return r

    c1 = C()
    # 1. version
    c1.raw(bytes([0]))
    check("version", c1.read(5) == bytes([58, 1, 0, 7, 0]))
    # 2-4. find live states + noservice
    r = find(c1, "a")
    h_a = struct.unpack("<I", r[2:6])[0]
    check("find-a", r[0] == 59 and r[1] == 2 and r[6] == 2, repr(r))
    r = find(c1, "b")
    h_b = struct.unpack("<I", r[2:6])[0]
    check("find-b", r[0] == 59 and r[1] == 0 and r[6] == 0, repr(r))
    c1.raw(bytes([1]) + struct.pack("<H", 4) + b"nope")
    check("noservice", c1.read(1) == bytes([60]))
    # 5. instance synthesis
    r = find(c1, "a@tty1")
    check("instance", r[0] == 59 and r[1] == 2, repr(r))
    check("instance-file", os.path.isfile(os.path.join(OUT, "a@tty1.conf")))
    check("instance-reload", len(reloaded) > 0)
    # 6-7. status generations
    c1.raw(bytes([18]) + struct.pack("<I", h_a))
    st = c1.read(12)
    check("status", st[0] == 70 and st[1] == 0 and st[2] == 2
          and st[8:12] == struct.pack("<i", 99), repr(st))
    c1.raw(bytes([28]) + struct.pack("<I", h_a))
    check("status6", len(c1.read(32)) == 32)
    # 8. list5
    c1.raw(bytes([25]))
    lb = c1.read(2 * 17 + 1)
    check("list5", lb[0] == 62 and lb[17] == 62 and lb[34] == 63, repr(lb))
    # 9. start lands with events+ALREADYSS; lagging stop answers ACK.
    c1.raw(bytes([3, 0]) + struct.pack("<I", h_b))
    e1 = c1.read(17)
    check("start-ev", e1[6] == 0, repr(e1))
    c1.read(21)
    check("start-lands", c1.read(1) == bytes([61]))
    c1.raw(bytes([4, 0]) + struct.pack("<I", h_a))
    check("stop-lags-ack", c1.read(1) == bytes([50]))
    c1.raw(bytes([3, 128]) + struct.pack("<I", h_b))
    check("preack", c1.read(1) == bytes([79]))
    e1 = c1.read(17)
    check("event", e1[0] == 100 and e1[1] == 17 and e1[6] == 0, repr(e1))
    e5 = c1.read(21)
    check("event5", e5[0] == 101 and e5[1] == 21 and e5[6] == 0, repr(e5))
    check("alreadyss", c1.read(1) == bytes([61]))
    # 10. stop on stopped -> events(ev=1) + ALREADYSS
    c1.raw(bytes([4, 0]) + struct.pack("<I", h_b))
    e1 = c1.read(17)
    check("stop-ev", e1[6] == 1, repr(e1))
    c1.read(21)
    check("stop-already", c1.read(1) == bytes([61]))
    # 11. close semantics
    c1.raw(bytes([23]) + struct.pack("<I", h_b))
    check("close", c1.read(1) == bytes([50]))
    c1.raw(bytes([23]) + struct.pack("<I", h_b))
    check("close-again", c1.read(1) == bytes([52]))
    check("close-eof", c1.read(1) == b"")
    # 12. dep persist through a fresh connection
    c2 = C()
    c2.raw(bytes([1]) + struct.pack("<H", 1) + b"a")
    fa = struct.unpack("<I", c2.read(7)[2:6])[0]
    c2.raw(bytes([1]) + struct.pack("<H", 1) + b"b")
    fb = struct.unpack("<I", c2.read(7)[2:6])[0]
    c2.raw(bytes([11, 2]) + struct.pack("<II", fa, fb))
    check("add-dep", c2.read(1) == bytes([50]))
    with open(os.path.join(OUT, "a.conf")) as f:
        check("add-file", "depends = after:b" in f.read())
    c2.raw(bytes([29, 2]) + struct.pack("<II", fa, fb))
    v7 = c2.read(32)
    check("enable-v7", v7[0] == 70 and v7[1] == 1 and len(v7) == 32, repr(v7))
    c2.raw(bytes([12, 2]) + struct.pack("<II", fa, fb))
    check("rem-dep", c2.read(1) == bytes([50]))
    c2.raw(bytes([12, 2]) + struct.pack("<II", fa, fb))
    check("rem-missing", c2.read(1) == bytes([51]))
    # 13. name/dscdir/loadmech
    c2.raw(bytes([15, 0]) + struct.pack("<I", fa))
    hdr = c2.read(4)
    n = struct.unpack("<H", hdr[2:4])[0]
    check("name", hdr[0] == 66 and c2.read(n) == b"a", repr(hdr))
    c2.raw(bytes([22, 0]) + struct.pack("<I", fa))
    dh = c2.read(5)
    dl = struct.unpack("<I", dh[1:5])[0]
    check("dscdir", dh[0] == 77 and c2.read(dl).decode() == OUT, repr(dh))
    c2.raw(bytes([13]))
    lm = c2.read(6)
    total = struct.unpack("<I", lm[2:6])[0]
    lm_rest = c2.read(total - 6)
    check("loadmech", lm[0] == 64 and lm[1] == 1 and OUT.encode() in lm_rest, repr(lm))
    # 14. env paths
    c2.raw(bytes([17]) + struct.pack("<H", 3) + b"a=b")
    check("setenv", c2.read(1) == bytes([50]))
    c2.raw(bytes([17]) + struct.pack("<H", 2) + b"=x")
    check("setenv-bad", c2.read(1) == bytes([52]))
    check("setenv-bad-eof", c2.read(1) == b"")
    # BADREQ closes the connection upstream too: fresh socket, same handles.
    c2 = C()
    # 15. allenv empty
    c2.raw(bytes([24, 0]))
    ae = c2.read(9)
    check("allenv", ae[0] == 78 and ae[1:] == bytes(8), repr(ae))
    # 16. catlog file + none
    c2.raw(bytes([20, 0]) + struct.pack("<I", fa))
    cl = c2.read(3)
    check("catlog", cl[0] == 73 and c2.read(cl[2]) == b"logged", repr(cl))
    c2.raw(bytes([20, 0]) + struct.pack("<I", fb))
    check("catlog-none", c2.read(1) == bytes([51]))
    # 17. signal paths (park b stopped first: prior sections move it).
    c2.raw(bytes([4, 0]) + struct.pack("<I", fb))
    c2.read(17)
    c2.read(21)
    assert c2.read(1) == bytes([61])
    c2.raw(bytes([21]) + struct.pack("<iI", 15, fb))
    check("signal-nopid", c2.read(1) == bytes([74]))
    c2.raw(bytes([21]) + struct.pack("<iI", 0, fa))
    check("signal-bad", c2.read(1) == bytes([75]))
    # 18. misc singletons
    c2.raw(bytes([7]) + struct.pack("<I", fa))
    check("unpin", c2.read(1) == bytes([50]))
    c2.raw(bytes([9]) + struct.pack("<I", fa))
    check("unload", c2.read(1) == bytes([51]))
    c2.raw(bytes([19]) + struct.pack("<I", fa) + bytes([1]))
    check("trigger", c2.read(1) == bytes([51]))
    c2.raw(bytes([16]) + struct.pack("<I", fa))
    check("reload", c2.read(1) == bytes([50]))
    c2.raw(bytes([27]))
    check("listenenv", c2.read(1) == bytes([50]))
    c2.raw(bytes([10, 4]))
    check("shutdown", c2.read(1) == bytes([51]))
    c2.raw(bytes([10, 2]))
    check("shutdown-halt", c2.read(1) == bytes([50]))
    c2.raw(bytes([10, 3]))
    check("shutdown-poweroff", c2.read(1) == bytes([50]))
    c2.raw(bytes([10, 4]))
    check("shutdown-reboot", c2.read(1) == bytes([50]))
    # 19. handle uniqueness across connections (close idle first:
    # one connection is served at a time, like upstream).
    c2.close()
    c3 = C()
    r = find(c3, "a")
    h3 = struct.unpack("<I", r[2:6])[0]
    check("unique-handles", h3 not in (h_a, h_b, fa, fb), (h3, h_a, h_b, fa, fb))
    # 20. unknown command closes
    c3.raw(bytes([200]))
    check("badreq", c3.read(1) == bytes([52]))
    check("badreq-eof", c3.read(1) == b"")
    c3.close()

    if fails:
        print("FAILURES: %s" % fails)
        sys.exit(1)
    print("proto: %d checks ok" % 43)
finally:
    proc.terminate()
