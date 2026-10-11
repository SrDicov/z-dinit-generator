# Iteración: shim dinitctl + fidelidad semántica (manual dinit como spec)

Jaula Chimera local, zinit v0.0.2 como PID 1 del namespace, 63 servicios.

## Running: 0 -> 3 (crond, tmpfiles-clean, udevd estable 1m56s pid 42)
Completados: early-dev-settle/devd/trigger, early-tmpfiles, ssh-keygen
(claves generadas), boot/system/local.target. Sonda reload-all: ok.

## Bloqueador 1 resuelto: shim dinitctl
Corpus usa SOLO `setenv` (env.sh, 5 llamadas con --use-passed-cfd).
Shim: setenv/unsetenv aceptados con aviso (exit 0), resto vía zctl,
trigger/wake fallan abierto. 16 checks en test/test-dinitctl.sh.
Vars de activación (CONTAINER/FIRST_BOOT) como snapshot en el env del
supervisor: early-env completa y el árbol avanza.

## Semántica corregida contra el manual dinit-service(5)
- depends-ms => required (milestone exige éxito; opt: lo toleraba).
- before => opt: invertido (orden sin cascada; required aparcaba
  pre-local tras uuidd, dinit no).
- restart ausente => omitir: dinit reintenta acotado 3/10s, zinit
  always es INFINITO (devmon llegó a 316 restarts). on-failure se mapea,
  en oneshot decae a never.
- Sin logfile => log = none (dinit descarta; el defecto file de zinit
  escribía /var/log en los 63).
- Sin ready-notification => ready = none (dinit arranca en exec; el
  defecto notify colaba 30 s por demonio). Con pipefd:N => notify.
- pipefd:N => preámbulo `exec N>&$ZINIT_NOTIFY_FD` (type script):
  verificado byte a byte, devmon escribió `READY=1\n` en fd 4.

## Frontera actual (una raíz lo para todo)
early-devmon necesita el protocolo dinit (libdinitctl over socket),
incluso en DUMMY_MODE: `failed to set up dinitctl`. Sin salida en C1.
device/early-devices.target/udevd/net-lo/... aparcan tras él, FIEL a
dinit (milestone fallido = dependientes parados).
Resto de raíces ambientales: root-rw+zram (montajes), uuidd/dbus/chronyd
(setuid a usuarios sin mapear en userns; en docker como root sí).
sshd: puerto 22 denegado en userns (real root sí).

## Nota de carrera (documentada, dirección segura)
devmon notifica (READY=1) ANTES de morir en el handle dinit; si la
ventana coincide, el milestone abre y el árbol avanza (udevd corre).
Si se pierde, todo aparca. Dirección segura en ambos casos.
