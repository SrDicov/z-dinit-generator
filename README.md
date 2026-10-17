# z-dinit-generator

Perfecto reemplazo de dinit sobre zinit. Un binario, std-only, cero
dependencias. Dos modos:

* **Modo 1 — conversión en vivo:** convierte tus servicios dinit ya
  existentes en servicios zinit funcionales.
* **Modo 2 — traducción en tiempo real:** se queda de fondo vigilando
  tus dirs dinit y re-traduce + `reload-all` sin convertir nada a mano.

```sh
sudo pacman -S zinit z-dinit-generator
sudo z-dinit-generator start   # convierte + compatd/watch
sudo zinit --sup /etc/zinit/services.d &   # supervisa junto a dinit
# paridad: todo nombre en `dinitctl list` (capturado ANTES) existe en `zctl list`
# 30 min estables, 0 failed; luego la ventana (sin rollback con estado):
sudo pacman -Rdd dinit          # quita el binario; el PID 1 sigue en RAM
# libera los puertos (dinitctl real vía SIGUSR1 si compatd tomó el socket),
# `dinitctl stop boot`, deja que zinit tome sshd/dbus, 1h de ventana
sudo reboot                     # con el bootloader ya en init=/sbin/zinit
# zinit como PID 1, dinitctl responde vía compatd, resto funcional
```

## Límites (lo que no traduce / no hace)

- `unload`, `untrigger`, `SOFTREBOOT`, `KEXEC`, `hibernate`: NAK honesto,
  no hay contraparte. `shutdown`/`reboot`/`halt`/`poweroff` SÍ actúan:
  van al supervisor (stop-all ordenado + deadline + breadcrumb).
- Demand-start: implementado para dependencias declaradas a servicios
  `enabled = no`: se activan automáticamente cuando el dependiente
  realmente corre (mirrors dinit `depends-on`). Servicios explicitamente
  parados por el operador (`zctl stop`) nunca se reactivan.
- `type = scripted` con protocolo start/stop → `oneshot` (el script
  corre una vez; sin comandos stop persistentes).
- `before:`/`after:` de dinit → ignorados con aviso; el orden zinit
  vive en `depends =` (+ `after:` solo-orden propio, otra semántica).
- `runs-on-console`, `starts-log`: aceptados, no modelados.
- OpenRC/systemd: fuera de alcance (solo dinit in, zinit out).

Texto dentro, texto fuera para convertir; demonio silencioso para vigilar:

```sh
z-dinit-generator convert /etc/dinit.d ./services.d
zinit check ./services.d
z-dinit-generator watch /etc/dinit.d ./services.d &
z-dinit-generator dinitctl status sshd
z-dinit-generator dinitctl list
z-dinit-generator dinitctl is-started sshd
z-dinit-generator dinitctl enable chronyd
z-dinit-generator dinitctl catlog sshd
z-dinit-generator compatd /run/dinitctl &
```

`dinitctl` cubre el set completo de `dinitctl(8)` (`start/stop/restart/wake/release/trigger/unpin/reload/add-dep/rm-dep/enable/disable/catlog/signal/is-started/is-failed/setenv/unsetenv`, con `--no-wait/--pin/--force/--ignore-unstarted/--quiet/--offline/--services-dir/--from`), espera por defecto como el original y falla en voz alta donde no hay contraparte (`unload`, `shutdown`, `untrigger`).

El `sh` histórico (`dinit-import.sh`, `dinitctl`) sigue ahí como
envoltorio fino; lo autoritativo es el Rust (paridad byte-idéntica en CI).
