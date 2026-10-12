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
z-dinit-generator compatd /run/dinitctl &
```

El `sh` histórico (`dinit-import.sh`, `dinitctl`) sigue ahí como
envoltorio fino; lo autoritativo es el Rust (paridad byte-idéntica en CI).
