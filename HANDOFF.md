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

## 2026-10-14 — Chimera FULL tree (72 svc) green under zinit

- Merged tree (usr 62 + etc) converts clean; musl v0.0.2 runs it.
  Static musl zdg BUILT LOCALLY (rustc --target
  x86_64-unknown-linux-musl, allowed by rules for the std-only
  generator; 5.1MB static PIE, matrix/zdg-musl-static) and deployed
  to the VM: compatd live + dinitctl symlink. TODO: CI musl-static
  artifact job so legs stop depending on a local binary.
- Results: 12 running (incl. dbus-daemon, udevd, sshd, dhcpcd→static
  net, polkitd, elogind, early-devmon, gettys), rest completed or
  correctly disabled; ANOM=0 silent-stuck. dinitctl shim proven on
  musl (is-started ×2, 71-svc list); early-env ambient crash FIXED
  by the shim (completed after 2 early fails).
- Classified, not chased: dinit-dbus (hangs talking to compatd —
  needs a focused protocol session), device/early-devmon %i
  instances (need compatd instance synthesis), zram-device (needs
  distro size config), wpa/mdadm/recovery (no HW / not demanded).
- dbus-sync dep on dinit-dbus dropped (bus itself is up; bridge
  tracked separately). notify-handshake services (polkitd,
  elogind) work fine — the earlier dbus notify scare was the
  print-address fd dance, Artix-side lesson stands.
- Ops: ssh-authorized_keys baked into ALL FOUR stagings (console
  FIFO is now backup only); static net.sh replaces blind dhcpcd
  on musl legs; `background:true` flaked repeatedly — setsid+&
  +disown in short calls is the reliable launcher; always verify
  qemu via /proc/$pid/exe (comm truncates, patterns self-match).

## 2026-10-14 — PACKAGING E2E green in Artix VM (the declared flow)

`pacman -U zinit + z-dinit-generator` → `z-dinit-generator start` →
`pacman -R dinit` → reboot via `/sbin/init` → zinit PID 1, 35
running, ANOM=3 (mdadm/recovery/wpa = dinit's not-loaded),
compatd+dinitctl live (94-svc list/status/is-started/restart),
TERM cascade with full announce. Matrix packaging state:
- New `matrix/pkg/zinit/PKGBUILD` (binary repack of the CI-green
  glibc artifact; packaging wraps tested bits, never rebuilds).
- Generator PKGBUILD: `provides=('dinit')` (dependents of -dinit
  subpackages stay satisfied so `-R dinit` works), dinitctl as an
  UNTRACKED post_install symlink (tracked files conflict with
  dinit-base — proven twice), zinit-init wrapper + /sbin/init flip
  in zinit.install post_install (restore on remove).
- Staging must NOT ship zinit bins (package owns /usr/bin/* or
  plain -U fails on untracked files — hit live).
- `start` regenerates everything from dinit sources: hand
  seasoning (dbus-direct, root-ro-target) gets CLOBBERED on every
  run — those two fixes belong upstream (backlog), seasoning does
  not survive `start` by design. `start` on 8f82fe2-era binary
  stacked dup enabled=no (E005, bricked a boot); 6bf0118 makes it
  a fixed point, proven live (reconvert → reload clean, 94 svc).
- dinit-watch has no future post `-R dinit` (watched dirs vanish)
  — it backing-offs forever; needs graceful-degrade or removal
  from the flow (backlog).
- ALWAYS verify installed sizes (`/usr/bin/z-dinit-generator`
  landed 0 bytes once from a suspect build; caught by --version).
- Rescue path proven twice: init=/bin/sh + FIFO + host loop-mount
  edits. FIFO console needs a persistent holder or qemu exits on
  stdin EOF; guest bash histexpand eats `!` (ssh for files).
- Remaining e2e gaps: bootloader-native reboot (QEMU uses
  init= override), `-S` from real repos (tested -U local), soak.

## 2026-10-14 — PACKAGING E2E closed (declared flow, in VM)

`pacman -U zinit z-dinit-generator` → `start` (94 svc) →
`pacman -R dinit` → reboot → zinit PID 1 via packaged /sbin/init
flip → 35 running, compatd+dinitctl live, TERM cascade. All in
Artix VM, all green:
- install/start/remove/reboot/init-selection/probe/compat/verbs.
- `z-dinit-generator reboot` verified (uptime reset; came back per
  QEMU cmdline as designed — bootloader-native reboot untested).
- Generator fixes landed for everything the e2e exposed:
  idempotent enabled=no (6bf0118), before-mirror yield (e577fc4),
  dotfile skip (8f82fe2), --print-address drop (f6c5e6e) — all CI
  green. e2e re-ran with fixed bits where it mattered.
- Packaging rules learned: staging must not ship package-owned
  bins; dinitctl via untracked post_install symlink (tracked files
  conflict with dinit-base); provides=('dinit') so -R works with
  -dinit dependents present; .install scripts need to be in
  source=(); ALWAYS verify installed sizes (0-byte ghost once).
- Rescue path used twice (init=/bin/sh + host loop-mount).
- Still open (not in VM): bootloader-native reboot, -S from real
  repos (packages unpublished), multi-day soak, cgroup-join root
  cause, dinit-dbus hang session, watch-vs-vanished-dirs,
  check-vs-sup unknown-dep gap. Next milestone per user: fix every
  open bug, then real remote-PC test.

## 2026-10-14 (later) — watch decimation froze cold boots (fixed c3b64c4)

`start` went unified first (c975f85: cmd_convert_dirs, one shared
graph, later layer wins by name) to fix a split-brain no-boot, but
cold boots still came up with 3 services (cronie/dbus/serial).
Bisected live in the Artix VM, same binary everywhere (md5
8aeb102e both sides):
- `convert /usr/lib/dinit.d` alone → full chain enabled (udevd,
  tmpfiles-setup, sysusers, local.target, system all clean).
- A second `convert /etc/dinit.d` over the same outdir decimates:
  etc owns its own `boot` file, so its closure runs over etc-only
  pulls and disables everything outside it. Systematic, not a
  shadowing bug (only `boot` is shadowed; pulls were complete).
- The live decimator was `dinit-watch` (cmd_watch ran per-dir
  converts on every fire, initial + on-change). Warm reloads hid it
  (Desired preserved); cold boots froze.
Fix c3b64c4: watch initial + re-fire go through cmd_convert_dirs.
Regression test: reconversion keeps a cross-layer closure. CI green
(ci + dinit-reference + chimera-e2e). Same-shape debt left in place:
reconvert_and_reload still converts a single parent dir.
Staging note: /etc/zinit/services.d is TRANSIENT (start/watch
regenerate it); cage-artix/ on host was deliberately NOT re-synced —
packaging regenerates at install.

## 2026-10-14 (later) — Artix cold census under zinit (proof4/proof5)

With fixed staging + cgroup-drop, cold boot: 94 supervised,
17 running (NetworkManager, acpid, agetty, bluetoothd, chronyd,
compatd, cronie, dbus, dhcpcd, dinit-watch, elogind, getty@tty5,
metalog, serial, sshd, udevd, userspawn), ANOM=0, SSH OK against
zinit PID 1. dbus runs from boot with no manual cgroup strip.
- One boot-time race: sysusers (oneshot esysusers, restart=never,
  log=none) died at boot → `suppressed` → local.target subtree
  (pre-network/network/dhcpcd/sshd/elogind/metalog) gated forever.
  Manual rerun exits 0; `kick` recovered the whole cascade.
  Root cause + frequency OPEN (needs reboot-with-log to catch it;
  log=none services die silently — observability gap, no death
  announce in the log). Suspected early lock/order race, not ro-root
  (/ stays rw: root-ro is out of closure by design).
- TERM-to-sup re-proven: 0 running + `no poweroff path, stays up
  and idle` announce. Note: the serial shell dies with the cascade,
  so FIFO goes dead too — recovery is relaunch, not re-login.

## 2026-10-15 — phantom writer caught: silent curl + old watch (no bug)

The proof4→proof5 "decimation" (70 enabled=no, 3 fresh mtimes) was
not nondeterminism: the guest never received the c3b64c4 binary.
`curl -s` failed silently (no route: manual IP lost on reboot,
dhcpcd gated) and the `;` before `/sbin/zdg start` ran the OLD
binary anyway. md5 proved it (guest 8aeb102e vs fixed 30478396…).
So the running watch was the pre-fix per-dir decimator, refiring on
real live mutations of the dinit dirs (7 spontaneous `reloaded:`
in proof4 — artix-live/userspawn-class tooling touches them).
Lessons: deploy with `&&`-chains + md5 assert, never `;`; give the
guest a persistent IP (seasoning) instead of per-session `ip` by
hand. Fix verified live: DEPLOY_OK, watch restarted to the new
image, `touch` on a dinit file refires (`reloaded`, unprompted)
with the chain intact (udevd/tmpfiles/sysusers/pre-local all stay
enabled). The 00:18 mtime dust was my own sed + old-watch writes.

## 2026-10-15 — proof9: sysusers race 3/3, VM left healthy

Fresh cold boot with the fixed watch live: staging intact, but
`sysusers` suppressed at boot again (3/3) and the network subtree
gated. `kick` recovered everything (sshd running). VM left up with
sshd/ssh working for tomorrow. The race is now the #1 Artix
blocker: `opt:` edges neither order nor block, so the early wave
runs fully concurrent; dinit serializes it via after/waits-for.
Missing primitive: order-only edges (design work, not a patch).

## 2026-10-15 — order-only `after:` lands, sysusers race closed

Root cause, proven: `root-ro` remounts `/` ro early, `early-root-rw`
remounts rw later (same dance as dinit), but with every ordering
edge mapped to `opt:` the whole tree spawned at t=0 into the ro
window and died at log-open (EROFS, pre-exec). Each failed spawn
eats budget, so `restart = never` services never retried and gated
their subtree 4/4 cold boots. A 60s sleep changed nothing (spawn
dies before exec); 20/20 manual runs pass.
Fix (zinit 428d6dd + fixes, generator 30795f3): `depends = after:x`
holds spawn until x's first attempt (started/completed/failed/
suppressed/disabled all release) without gating on outcome.
`after`/`waits-for`/before-mirror all emit `after:`; closure
unchanged (waits still pull, after/before never do); after-cycles
fatal at load like required ones; dangling after warns (W102).
CI fallout fixed along the way: parser E010 fired on fresh
required entries, clippy err_expect, rustfmt; e2e now consumes
CI-green testing artifacts (glibc host check, musl container
boot/check — check runs in-container because user= only resolves
where the packages are) until the next release re-pins a version;
musl artifact now ships zctl too. dinit-import.sh + proto test
moved to `after:` with the Rust side (parity kept).
Validated live (proof14): sysusers/local/pre-network completed at
boot, sshd+dhcpcd running, 17 running, ANOM=0, SSH ok — zero
interventions. Deploy note: new confs brick old sups (E008), so
ship binary+staging together and reboot (ETXTBSY: install the
running sup via rename, never direct overwrite).

## 2026-10-15 — proof18: remount race + cgroup join both closed live

New sup (oneshot-completion gate + NUL fix + step announce) with
regenerated staging, zero interventions from cold boot: sysusers
completed (waits early-root-rw's *completion*, not its start —
releasing on Starting raced the mount syscall itself), dbus running
first try WITH `cgroup = dbus.sv` (restarts 0/3), process confirmed
in `0::/zinit.slice/dbus.sv`. 17 running, ANOM=0, SSH ok.
Refinement on the way in: failed/suppressed/disabled always release
(no hangs, waiter rides alongside retries); mixed required+after
loops are load-fatal like pure cycles. Both repos all-green.

## 2026-10-15 — cgroup join root cause: NUL padding (found by reading)

Every `cgroup =` service died pre-exec (exit 127) while manual joins
worked and cgroup2 presence never mattered. Cause: `pid_ascii` fills
right-to-left, but `child_step_cgroup` wrote `buf[0..len]` — NUL
padding the kernel rejects with EINVAL. One-line fix via a named
`pid_range` helper (+test); test helper reuses the new public
`child_step_name` instead of its own table. Generator re-emits
`run-in-cgroup` (Chimera corpus uses none, so e2e is safe; Artix
dbus will prove it live). Enabler alongside: `SpawnFailed` now
carries the pre-exec step (0 = refused before fork) and the sup
announces `child failed at <step> (<err>)` — `poll_exec` used to
discard the step word. Same session also fixed `reconvert_and_reload`
(single-parent converts decimated like the old watch).
