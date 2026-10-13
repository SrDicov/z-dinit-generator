# z-dinit-generator

Perfecto reemplazo de dinit sobre zinit. Un binario, std-only, cero
dependencias. Dos modos:

* **Modo 1 — conversión en vivo:** convierte tus servicios dinit ya
  existentes en servicios zinit funcionales.
* **Modo 2 — traducción en tiempo real:** se queda de fondo vigilando
  tus dirs dinit y re-traduce + `reload-all` sin convertir nada a mano.

```sh
sudo pacman -S zinit z-dinit-generator
sudo z-dinit-generator start
sudo pacman -R dinit
sudo reboot
# todo sigue funcional: zinit como PID 1, dinitctl responde, devmon callado
```

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
