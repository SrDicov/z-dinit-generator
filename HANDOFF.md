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
