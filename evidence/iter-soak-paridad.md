# Iteración: soak 30 min + paridad dinit-en-docker + sobrevive

## Soak local 30 min, resolución 1 min (cage userns, zinit v0.0.2)
- running 3 (crond, udevd, tmpfiles-clean) x7 muestras, luego 2:
  tmpfiles-clean salió exit 0 tras su ciclo (sleep 900 + clean):
  ciclo de vida diseñado, NO fallo. Tras stop/start vuelve a 3.
- kick devmon + reload-all a mitad: sin disrupción (2 estables).
- down/up de crond/udevd/tmpfiles-clean: vuelven a 3.
- sup RSS 169 -> 260 tras reload-all (one-time +364 KB), plano 20 min,
  luego 196 (no monótono: allocator, no leak). init 86 clavado.
- fds 76 -> 74 -> 76 (+pico 85 una muestra, en vuelo) -> 76: sin fuga.
- zombies 0/30. procs 6/4/5 coherentes. SOAK LIMPIO.

## Bug del harness hallado y corregido (evidencia, no silencio)
Soak v1 murió sin una línea: `r=$(... | grep -c ...)` con 0 matches
sale 1 y `set -e` mata. v2 sin -e, todo guardado,log con huecos.
La lección queda en el script, no en un postmortem.

## Paridad dinit-en-docker (job dinit-reference, mismo corpus)
dinit real: cascada de [FAILED] desde early-pseudofs (montajes),
0 demonios, Y SIN SOCKET (/run/dinitctl inexistente: dinit lo crea
al tener / gestionado; en contenedor nunca). zinit mismo contenedor:
socket vivo, 6 demonios Running, resto en espera sin cascada.
chronyd falla IGUAL en ambos (entorno, no traductor).
En degradado, zinit degrada mejor; documentado, no alardeado.

## Docker como root, traductor final (6 Running)
crond, dbus-daemon (fd-bridge + notify, 2/3 acotado), sshd (2/3),
tmpfiles-clean, udevd, uuidd. Completados: boot, system,
early-cryptdisks y toda la rama scripted sana.
Aparcados fieles: device-subtree (devmon exige protocolo dinit),
root-rw/zram (montajes), resto por cascada correcta.

## Puertas que solo abre el metal (no girar más contenedor aquí)
1. login real con getty (sin tty no hay prueba; runs-on-console sin
   destino C1: el operador escribe el getty a mano).
2. stop-command/chain-to con portador Running (todos sus servicios son
   early; en contenedor nunca corren largo).
3. Apagado con huérfanos reales (en contenedor ya limpio: 2 procs).
4. HW: udev con hardware, mounts, hwclock, módulos, swap real.
Procedimiento metal archivado en el repo cuando haya máquina.
