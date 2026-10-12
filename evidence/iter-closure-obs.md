# Iteración: cierre perezoso observado en jaula

## Comportamiento nuevo (correcto)
sshd/crond/uuidd: `stopped desired=down` de arranque. boot completed.
`zctl start crond` → Running en 5 s (pid vivo). Las dos mitades del
contrato dinit (apagado por defecto + arranque explícito) funcionan.
dinit jamás cargaba sshd (ausente en su lista); celebrarlo Running
era la divergencia.

## 8 fuera / 55 dentro (corpus jaula)
Fuera: crond, device, recovery, single, ssh-keygen, sshd, uuidd,
zram-device. Todos verificados ausentes o inactivos en el dinit real.
device nunca arranca estático (lo instancia devmon por protocolo);
ssh-keygen solo corre si sshd lo arrastra (nunca, apagado).

## Nota de técnica (para la fase VM)
La jaula userns deja todo uid 1000/nobody en disco: NO sirve como
disco VM. Reconstruir como root real (doas chroot + apk fix) antes
de virtualizar, o los setuid/propietarios llegan rotos.
