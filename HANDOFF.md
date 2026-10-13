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
