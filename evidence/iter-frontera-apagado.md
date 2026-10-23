# Iteración: fidelidad total + frontera de una raíz + apagado limpio

## Mapa de bloqueo (programa Frontier, 63 status vía zctl)
sshd, dbus-daemon, chronyd, crond, udevd, uuidd, local.target,
pre-local.target: TODOS paran en early-devmon. Única raíz rota con
dependientes. zram-device roto sin dependientes. El resto, espera
correcta en cascada.

## Puente de readiness pipefd:N verificado byte a byte
devmon manual con fd 4 cableado escribió `READY=1\n` (DUMMY_MODE=1).
El traductor emite preámbulo `exec N>&$ZINIT_NOTIFY_FD` (type script,
antes del cd: `exec cd` reemplazaría la shell). Regla any-byte de
zrt lo consume. dbus (--print-address=4) y chronyd (pipefd:11) usan
el mismo puente; su crash posterior es por setuid, no por readiness.

## Reversión documentada: restart ausente => omitir (NO always)
zinit `always` es ILIMITADO (devmon llegó a 316 restarts, loop caliente
eterno); dinit por defecto reintenta acotado 3/10s. Omitir da on-failure
acotado 5/60: la envolvente acotada coincide, el caso exit-0-limpio de
un demonio sano no existe en el corpus. Esquinas: on-failure=>igual,
en oneshot decae a never (manual dinit).

## Referencia dinit-en-docker (job dinit-reference, CI)
dinit real tampoco levanta nada en contenedor: cascada de [FAILED]
(pseudofs sin montajes arrastra tmpfs/devd/machine-id/hwclock/...,
local.target, dbus, chronyd...), 0 demonios, y SIN socket de control
(/run/dinitctl inexistente: dinit lo crea al tener / rw gestionado).
zinit en las mismas condiciones: socket vivo, 3 Running, resto en
espera (no cascada de fallo). En degradado, zinit degrada mejor.
dinit ante SIGTERM = apagado total (init 1 lo reenvía); zinit --sup
SIGTERM = parar todo y seguir idle. Divergencia registrada para el
punto 8 (apagado en metal lo decide el operador, no el shim).

## Apagado limpio local (evidencia)
PRE: 6 procs, todos S, 0 zombies. stop x63 en orden inverso.
POST: 0 running, 2 procs en jaula (init+sup), 0 zombies, sin SIGKILL.
stop-command (7) y chain-to sin verificar: ningún servicio que los
usa llegó a Running (todos early, aparcados/rotos por entorno).

## Paredes de entorno del userns (no son bugs, valen en docker/metal)
- setuid a usuarios sin mapear: uuidd/dbus/chronyd no pueden correr aquí.
- bind <1024: sshd puerto 22 denegado (claves sí generadas por ssh-keygen).
- mounts: root-rw/zram/pseudofs. tmpfiles sí crea /run/dbus y /run/uuidd
  (early-tmpfiles completed): el sistema se auto-prepara donde puede.
