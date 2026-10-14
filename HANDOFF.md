# HANDOFF — resume on a bigger PC

Clone/pull, everything is on `testing` branches, CI green.

## Repos (both: work on `testing`, `main` only via draft PR + approval)

- `SrDicov/z-dinit-generator` @ `571288b` — PR #1 (draft) tracks testing.
- `SrDicov/zinit` @ `8c9ea00` (testing = main + fuzz-bin fix).

## What lives where (this PC: `/home/dicov/`)

- `z-dinit-generator/`: `dinit-import.sh` (translator), `dinitctl`
  (shim), `compatd.rs` (std-only Rust, dinit-protocol server, 4 tests),
  `.github/` (ci, chimera-e2e, dinit-reference), `evidence/iter-*.md`.
- `inits/chimera/` (112 MB): Chimera userland cage (apkupati 111 pkgs).
- `inits/chimera-vm.raw` (180 MB sparse): ext4 disk from the cage,
  ownership fixed (`apk fix` + top-level chown), boots under QEMU/KVM
  with zinit as PID 1 (see `evidence/iter-vm-login.md` for the exact
  kernel/initrd/cmdline recipe; kernel+initrd live in the ISO below).
- `inits/chimera-live-base.iso` (1 GB): official Chimera x86_64 base.
- `/tmp/opencode/`: throwaway helpers only (pull-oci.py, vm-con.py
  relay+mailbox, qmp-type.py, soak.sh, frontier.py). Re-create, don't
  depend on them.

## Proven state (evidence/ has the logs)

- 63/63 services translate, `check` green; 6 daemons Running as root
  in docker (crond, dbus, sshd, tmpfiles-clean, udevd, uuidd) +
  uuidd/sshd on-demand; real `login:`→`#` in KVM VM; soak 30 min
  clean; shutdown 66/66 + 0 zombies (cage).
- compatd: compiles + 4/4 tests in CI. NOT YET wired: service confs,
  early-devmon drop-in, e2e proof (devmon Running). That's the next
  task, then VM re-run for the device subtree.

## Open upstream (zinit repo)

- PR #1 (generator), issues #19 (RV32 blocked-by-upstream),
  zinit#23 (udevd generations vs restarts counter + stuck pid 121).
- zinit v0.0.2 released (note: version string still 0.1.0, cosmetic).

## Resume commands (new PC)

```sh
gh repo clone SrDicov/z-dinit-generator && cd z-dinit-generator
git checkout testing
# docker path needs nothing local: push runs chimera-e2e in Actions.
# VM path needs: qemu-system-x86_64, KVM, the .raw + kernel/initrd
# recipe from evidence/iter-vm-login.md.
```

---

# 2026-10-13 — Alpine leg CLOSED, orphan-reap bug fixed (local matrix)

Repos (both `testing`, both pushed, CI green):
- `SrDicov/zinit` @ `714148f` — ci · alpine-e2e · distro green.
- `SrDicov/z-dinit-generator` @ `0b74022` — ci · chimera-e2e ·
  dinit-reference green. Untouched this session.

Matrix lives in `/home/dicov/matrix/` (scratch, no repo):
`disk-alpine/` (VM rootfs staging), `vm-alpine.raw` (ext4 disk via
`mkdisk.sh`), `cage-alpine/` (jail), `zdg-cur/` (generator `0b74022`),
`zcur/` (zinit glibc `f9a8fdb`), `cascade.log` (boot+shutdown serial
capture), `vm-alpine-zinit.log` (older `-nographic` boots).

## Alpine VM recipe (working, repeated 6+ boots)

```sh
qemu-system-x86_64 -m 1024 -smp 2 -enable-kvm \
  -kernel disk-alpine/boot/vmlinuz-virt \
  -initrd disk-alpine/boot/initramfs-virt \
  -append "root=/dev/vda rootfstype=ext4 rootwait rw console=ttyS0,115200 init=/sbin/zinit-init" \
  -drive file=vm-alpine.raw,if=virtio,format=raw \
  -netdev user,id=n1,hostfwd=tcp::2222-:22 -device virtio-net-pci,netdev=n1 \
  -display none -serial unix:ser.sock,server,nowait
```

Services (`disk-alpine/etc/zinit/services.d/`, 5): `boot` (oneshot
true), `net` (oneshot `/sbin/net.sh`: static 10.0.2.15/24 via
10.0.2.2), `serial` (getty ttyS0), `sshd` (script, `-D -E /sshd.log`,
depends `net`), `probe` (script, `zctl` dump to `/probe.out`,
depends `boot`). PID 1 = `/sbin/zinit-init` wrapper (see below).

Evidence per boot: `login:` on ttyS0, ssh root@localhost:2222 OK
(PID1=zinit), `/probe.out` with PROBE_DONE, `zctl list` =
`boot/net/probe stopped + serial/sshd running`, 0 zombies,
`zctl` verbs list/status/stop/start/kick/restart/reload-all green,
kill -9 sshd rearms (restarts=1/3), 16-min active soak, TERM to
`--sup` stops everything (machine idle, no poweroff path by design).

## Two real finds this session

1. **VM root mounts ro, nothing remounts it** (Alpine initramfs
   ignores `rw`; EXT4 log line proves `ro`). sshd `-E /sshd.log`
   and every `> /file` writer died silently → backing-off +
   missing probe.out. Fix in staging (no core change — the dep gate
   only promises "dep spawned", never "dep's effects done", same as
   dinit): `disk-alpine/sbin/zinit-init` runs
   `mount -o remount,rw /` before `exec zinit --init`. Ordering by
   construction. Lesson: side-effect ordering belongs
   pre-supervisor or behind completed-oneshots, never behind a
   `Running` gate. Also fixed `mkdisk.sh` (fstab was overwritten,
   now appended) and `probe.conf` oneshot→script (oneshots split on
   whitespace, shell never ran).
2. **zinit leaked reparented orphans** (`714148f`, own commit): 3
   `sshd-session` zombies under `--sup` after killing/restarting
   their listener mid-session. Live listener reaps fine (1 clean
   session → 0 new zombies), so supervisor-side: `PidfdTracker`
   only drains tracked pids. Fix: `waitpid(-1)` sweep in
   `Supervisor::reap` (unknown pids hit the already-silent
   untrack; no new log paths). First attempt inside the tracker
   broke 2 unit tests on all CI targets (shared-process sweep
   steals siblings' children) → moved up a layer + regression
   test appended to `supervises_real_service.rs` (double-fork
   service, ESRCH assertion). Stuck on musl: Alpine runs frozen
   v0.0.2 (CI builds glibc only), so the VM fix lands on the next
   musl release; glibc legs (Void/Chimera/Artix) get it now.

## Shutdown contract (verified, read before testing it)

- TERM to PID 1 (`--init`) is **ignored by design** (`init.rs`:
  power signals queued, never forwarded). Kill -TERM 1 and sshd
  stays up — that is correct behavior, not a bug.
- TERM to `--sup` (=1020-ish, `ps -eo pid,args | grep
  '[z]init --sup'`) → `SIGTERM received: stopping...` on console,
  every service down, machine idle. No poweroff/reboot path
  exists (by design); machine-off = hypervisor. Final console
  announce line arrived truncated on the shared getty console
  (cosmetic wart, noted, not investigated).
- Never `pkill -f <pattern>`: it matches its own `bash -c` wrapper
  (this session) — `pkill -x` or exact-PID kills only. SIGTERM
  never kills a supervisor either (idle, not dead) — use SIGKILL
  by PID for teardown, and `kill -9` the exact qemu pid or the
  next `mkdisk` fails on the image lock.

## Pending (untouched, prepped only)

- **Void**: cage has dinit+openssh+dracut, kernel 6.18.56_1 +
  initramfs built, sshd converted to double quotes (old crash-loop
  was the single-quote tokenizer bug, fixed `584f708`). Next:
  re-run dinit-reference VM, then convert + zinit-PID1 boot.
- **Chimera**: 61 services in `cage-chimera/usr/lib/dinit.d/`,
  kernel present, **no initrd** — needs initramfs before any VM.
- **Artix**: ISO re-verified byte-exact (1236064256, md5 of
  rootfs.img `b5353c5c...` matches), kernel 7.1.8-artix1 +
  initramfs extracted to `iso-artix/boot/`, rootfs unsquashed to
  `cage-artix/` (xattr warnings as non-root, harmless). Next:
  season + `mkdisk.sh` + boot.
- **Verb matrix**: `zctl` done (v0.0.2 verbs only — no
  pin/unpin/log there); compatd/dinitctl need glibc → run them in
  Void/Chimera VMs, not Alpine (zdg is glibc-dynamic).
- **Divergences**: one clarified — dep gate = "dep Running",
  same as dinit's started-semantics; the full list still needs the
  break-the-corpus-or-accept classification.
- Old `qtype.py`/`vsh.py` serial helpers superseded by direct
  socket python + ssh; `ser.sock` needs an early-connected logger
  or early boot output is lost (unix socket, no replay).

## 2026-10-13 (later) — Void leg CLOSED (dinit reference + zinit PID 1)

Recipe: kernel+initrd from `cage-void/boot/`, `-append
"root=/dev/vda rw ip=dhcp rd.neednet=1 console=ttyS0,115200
init=/usr/bin/dinit|/sbin/zinit-init"`, `-nographic < fifo` for
interactive console (unix-socket serial backend went silent twice;
FIFO never failed), ssh via `-hostfwd=tcp::2224-:22`.

- dinit reference: `[OK] sshd/serial/boot`, `dinitctl list` =
  boot+serial+sshd, sshd STARTED pid 586, ssh OK, 0 zombies.
  Needed: `dracut-network` package + `rd.neednet=1` (dracut skips
  net for local root otherwise), `40network` confirmed in image
  (lsinitrd does NOT exist on this host — inspect via
  `zcat | cpio -t`), sshd_config.d/empty.conf (root+empty).
- zinit PID 1 (fresh glibc bins @714148f with the orphan fix):
  `supervising 4`, probe PROBE_DONE, sshd/serial running, ssh OK.
- Root cause (2nd of the day): Void's shipped sshd dinit file runs
  the daemon **without `-D`** → classic double-fork: parent exits
  0 instantly, untracked daemon holds :22, supervisor respawns
  forever (`exited with status 0`, restarts=81). dinit tolerates
  the same file (stable [OK] — restart-on-unexpected-only,
  divergence-list candidate). Fixed at source (`-D`, one word),
  re-converted, stable 4+ min restarts=0/3. GENERATOR NOTE: no
  static detector for daemonizing commands exists; `-D` is a
  seasoning rule for sshd-class services.
- Orphan fix proven in production: kill -9 listener mid-session →
  respawn tracked+running, **0 zombies** (old binary left 1).
- compatd live-deployed (no rebuild: conf via ssh +
  reload-all): `/sbin/zdg dinitctl list/status/is-started/stop/
  start` all green over the real v7 socket. `kick` on a
  budget-backing-off service answers "nothing to kick" (v0.0.2-era
  semantics; kick only clears suppressed/completed).
- Shutdown: TERM to `--sup` (NOT PID 1 — `--init` ignores power
  signals by design) → full announce line on console, sshd down,
  machine idle. The Alpine truncated-announce was a socket
  artifact, not a product bug.
- NEVER `pkill -f <pattern>`: killed its own wrapper and orphaned
  a live qemu twice this session. Exact-PID kills + pre-kill
  `cmdline` verification only.

## 2026-10-13 (later) — Chimera leg 90% (VM boots, needs ssh+shutdown proof)

Recipe: kernel `cage-chimera/boot/vmlinuz-6.18.48-0-generic` + HAND-ROLLED
`initramfs-chimera.img` (`./mkinitramfs-chimera.sh`: Chimera ships no
dracut/mkinitfs and virtio_blk/ext4 are =m; ~30 files: musl sh+tools,
kmod, module closure from modules.dep, /init that modprobes, waits for
/dev/vda, switch_roots to `init=` from cmdline). Same `-nographic <
qemu-in` FIFO console pattern as Void (unix-socket serial went silent
twice; FIFO never failed). `-hostfwd=tcp::2225-:22`.

- Gotchas baked into `mkinitramfs-chimera.sh`: musl loader must land on
  the exact INTERP path (a stray `]` from readelf broke the first image
  → kernel panic ENOENT); kmod needs the BINARY indexes (dep.bin et
  al., not just text dep) or every lookup fails "not found"; /init
  needs `export PATH` (only /bin is default) + step echos.
- Seasoning: minimal dinit sources (serial via distro
  `/usr/lib/agetty-service ttyS0` — Chimera uses nyagetty, installed
  `nyagetty nyagetty-dinit`; sshd distro file already `-D`, added
  `mkdir -p /run/sshd /var/chroot/ssh` — Chimera privsep dir is
  non-standard; root unlocked; host keys via ssh-keygen -A after
  mkdir /etc/ssh; empty-password sshd_config.d), convert clean,
  `net` = REAL distro dhcpcd service (installed `dhcpcd
  dhcpcd-dinit`; `before: network.target` warns+ignores correctly).
- Convert policy found: services outside boot's dinit closure get
  `enabled = no`, and EVERY convert run APPENDS it again (no
  overwrite, no dedupe) — always `rm services.d/*.conf` before
  re-converting, then re-apply seasoning. This explained the "v0.0.2
  boot quirk" (net/probe never auto-started): my own stale files,
  not a supervisor bug. GENERATOR BACKLOG: idempotent convert.
- Closed 2026-10-13: static net.sh oneshot (dhcpcd needs carrier-up
  ordering/udev, unsuitable headless — one line, moving on), all 5
  auto-start at boot, ssh into 2225 OK, `zctl`
  kick/reload-all/restart + `kill -9`-rearme pattern inherited from
  Void, 0 zombies, TERM-to-sup stops everything, stays down, no
  relaunch (exactly one `supervising` line; v0.0.2 prints no
  announce line — cosmetic, proven present on current source in the
  Void leg). Full-trace gotchas (loader `]`, dep.bin, PATH, api-fs
  --move) all baked into `mkinitramfs-chimera.sh`. Musa note: Chimera runs frozen
  v0.0.2 musl like Alpine (orphan fix pending musl release).
- Deferred deliberately: full 61-service tree in VM (CI chimera-e2e
  covers convert+container); early-env (needs dinitctl shim =
  glibc, won't run on musl — same constraint as Alpine compatd)
  and root-rw mounts belong to that full-tree effort.

## 2026-10-13/14 — Artix leg CLOSED. MATRIX 4/4 COMPLETE (minimal plans)

Recipe: kernel `iso-artix/boot/vmlinuz-x86_64` (7.1.8-artix1), NO
initramfs (virtio_blk/pci+ext4 built-in; virtio_net is a module →
`modprobe virtio_net` in `zinit-init` wrapper, pre-supervisor),
`root=/dev/vda rw console=ttyS0,115200 init=/sbin/zinit-init`,
`-nographic < qemu-in` FIFO console, `-hostfwd=tcp::2226-:22`.
4G image (2.1G rootfs + ext4 overhead exceeds the 2G default;
a stale loop-mount from a failed mkdisk must be umounted by hand
before retrying).

- Full-tree convert (42 files) works with the e577fc4 before-mirror
  fix (logind E010 gone); `check` green modulo W003. Curated plan:
  boot/serial/sshd/dhcpcd/probe + live-added compatd (+static net
  oneshot; dhcpcd kept running harmlessly).
- Seasoning: root unlocked, host keys via ssh-keygen -A (needs
  cage-artix/dev/null touched first as bind target), empty-password
  sshd_config.d, serial=agetty ttyS0, sshd=distro `-D` + run/sshd
  mkdir, zinit/zctl glibc FRESH (714148f, orphan fix included),
  probe script, compatd live-deployed (no rebuild).
- Results: login+root shell, PID1=zinit, ssh OK, probe PROBE_DONE
  at boot, 0 zombies, dinitctl list/status/is-started/restart +
  zctl kick/reload-all green over the real v7 socket, TERM-to-sup
  cascade with FULL announce line, stays down, no relaunch.
- Gotchas: `ip=`/`rd.neednet` N/A (no initramfs); dhcpcd blind
  without udevd (same as Chimera — static chosen); guest bash has
  histexpand ON — multi-line FIFO writes with `!` die with "event
  not found", and `set +H` must travel on its OWN line; prefer
  ssh (non-interactive, no expansion) for all file writes once it
  exists; FIFO only for pre-ssh bootstrap. `head -n 1` can print
  empty on a broken-shebang file — verify with cat -A.
- Generator fix this leg (e577fc4, CI green): before-mirror yields
  to an existing required edge (was: fatal E010 on Artix logind).
  Backlog filed by observation: convert not idempotent (appends
  enabled=no; never overwrites generated files either — always rm
  + reconvert + re-season), `check` does not flag unknown required
  deps while sup fatals on them (NetworkManager/dhcpcd
  pre-network.target), no static detector for daemonizing commands
  (Void -D saga), no `type=target` emission for dinit .targets.

MATRIX STATE: Alpine closed, Void closed (ref+dinit+zinit+compatd),
Chimera closed (5svc, ssh, verbs, shutdown), Artix closed (6svc,
ssh, compatd verbs, shutdown). All on testing, all pushed.
Deliberately open: full 61-svc Chimera tree in VM (CI container
covers it), divergence classification list, packaging e2e installs,
remaining dinitctl verbs (catlog/signal/enable/disable), per-VM
15-min soaks (only Alpine did the full soak).

## 2026-10-14 — Artix FULL tree under zinit (92 svc, in triage)

- Merged tree (usr/lib base + etc overrides) converts clean with
  8f82fe2 (before-fix) + dotfix; `check` exit 0. Staging =
  out-full + probe + rclocal-target + root-ro-target +
  dbus-direct (no cgroup/dance). Distro `boot` restored (system +
  waits boot.d) + serial; serial/sshd/dhcpcd/probe in closure.
- Live result: 92 loaded, ~15 running, STUCK_NEVERRAN=0, 0 errors.
  Chain unlocked stepwise: rclocal-stub → 24, dbus-direct →
  elogind/logind/avahi/cupsd/bluetoothd/NM. Key daemons all
  Running with restarts≈0 (dbus, udevd, elogind, sshd, dhcpcd,
  NetworkManager). dhcpcd leases 10.0.2.15 (needs udevd up).
- Root causes nailed: (1) root-ro remount,ro always fails in VM
  (busy) → stubbed as target (fsck already pre-PID1); (2) dbus
  --print-address=4 fd dance incompatible + cgroup= join breaks
  spawn — direct binary runs; notify-protocol mapping + cgroup
  join stay in backlog; (3) `enabled=no` closure is CORRECT
  (≈dinit lazy demand): stripping it caused wpa/mdadm/recovery
  churn; with closure they stay disabled exactly like dinit's
  not-loaded. dinit-ref on same image: 65 loaded, all started.
- Open threads (resume here): (a) set-diff dinit-65 vs zinit-61
  (dl2.txt pending inside the dinit VM log — VM was killed before
  reading it; reboot dinit-ref and dump `dinitctl list`); (b) CI
  verdict on 037fc35+zcore-target-fix (fmt fixed c4672a8, watch
  for green); (c) wpa/mdadm/recovery parity (dinit: not loaded);
  (d) packaging e2e + divergence list still untouched.
- Ops lessons: FIFO needs a persistent holder or qemu exits on
  stdin EOF; `background:true` launches flaked twice — setsid+&
  +disown in short calls is reliable; never trust accumulated
  *.log (one file per boot); guest bash histexpand eats `!` —
  ssh for file writes, FIFO for bootstrap only.

## 2026-10-14 — Artix FULL tree (92 svc) green under zinit

- Merged tree (usr/lib 50 + etc 45) converts clean with 8f82fe2;
  `check` exit 0. Seasoning: probe, rclocal-target, root-ro-target,
  dbus-direct (no cgroup/dance), getty@tty1-5 enabled (dinit runs
  them via agetty script; static enable = same set), distro boot +
  serial (pulls `system` + boot.d soft set into closure).
- Result: 20 running, rest completed/disabled, ANOMALOUS=0
  (machine-checked: no silent-stuck, no unexpected failure).
  Key daemons Running restarts≈0: dbus, udevd, elogind, sshd,
  dhcpcd (leased .15), NetworkManager, bluetoothd, cronie,
  metalog, 5 gettys. wpa/mdadm/recovery disabled-by-closure =
  dinit's not-loaded (parity, proven on dinit-ref same image).
- Set-diff dinit-65 vs zinit-61: only getty@tty1-5 differed
  (fixed by enabling) + probe/rclocal (ours). Everything else
  matches 1:1, including udevd/dbus/NetworkManager/bluetoothd.
- dinit demand model confirmed lazy (loads on demand from boot);
  zinit closure+bfs matches it for this tree. Remaining zinit
  gaps filed: cgroup= join breaks spawn, notify protocol vs
  dinit pipefd:N unmapped, convert idempotency, check-vs-sup
  strictness on unknown deps, no type=target emission.
