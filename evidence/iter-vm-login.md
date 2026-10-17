# Iteración: login real + apagado en VM + zombie archivado

## MURO 5: login de verdad en metal virtual (KVM, mounts reales)
getty transplantado (agetty+login+pam del host, misma musl) en ttyS1,
segundo serial + relay con buzón para full-duplex, root:test vía
openssl-hash en shadow. Secuencia observada en serie:
`login:` → `root` → `Password:` → `#`, `id` => uid=0(root).
getty supervisado estable (0 restarts). Regla de arnés: jamás parar
dbgsh en un stop-all (mata el único lector del serie).

## Arranque perezoso en VM: sshd/uuidd/crond down por defecto (enabled),
## `zctl start` los levanta (uuidd pid 363 restarts 0; sshd tras
## ssh-keygen on-demand: pid 445 restarts 0). dbus se auto-curó por
## refill acotado tras aparecer /run/dbus (snapshot zlist: 6 Running).

## Apagado: 66/66 stops con rc=0, 0 Running en state/ persistido,
## jaula previa: 2 procs, 0 zombies. Sin SIGKILL.

## Zombie 121 archivado upstream (zinit#23), no silenciado
udevd cambia de pid (42>121>132>1095>133) con restarts siempre 0 y
121 (ppid=sup) clavado 15+ min. Sin OOM. Stop/start limpio no lo
reproduce. Candidato: muerte espontánea sin contabilizar ni reaping
(fast-death pre-track o respawn sin registro). Repro en el issue.
No bloquea los 10 puntos (cage soak 0/30); queda como issue abierto
igual que RV32#19.
