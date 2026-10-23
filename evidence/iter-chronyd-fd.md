# Iteración: chronyd hasta la causa raíz + puente fd probado + soak

## chronyd: `Fatal error: Could not send notification requested by -N`
-N FD de chrony = notificar en ese fd (pareja de pipefd:11 del fichero).
Fallo en contenedor por DOS capas, separadas con experimento:
1. Sin puente (mando pelado): EBADF esperado, fatal. Mi primer repro
   manual era inválido (sin fd 11 cableado): no decía nada del caso real.
2. Con puente en shell capaz (bash en contenedor desechable): chronyd
   arranca SIN fatal y escribe `\n` en el pipe. Puente VÁLIDO.
3. PERO: `\n` solo es línea vacía y la regla any-byte exige byte no
   blanco (ready.rs:259, `all` vacuo = true): NO declara ready.
   chronyd quedaría en timeout-30s-degradado, nunca instantáneo.
4. Y ADEMÁS: /bin/sh stock Chimera no redirige fds de 2 dígitos
   (`exec 9>&1` ok, `exec 11>&1` => `11: not found`). Probado directo.
   En stock, el preámbulo muere antes que chronyd: crash-loop.
Conclusión: puente correcto; chronyd necesita shell capaz para N>9
(o bash en el sistema) y aun así solo alcanza ready por timeout.
Avisado en el header del traductor. dbus (fd 4, dirección no vacía)
sí alcanza ready instantáneo: corre con 2/3 acotado. devmon escribió
`READY=1` explícito: instantáneo también (luego muere en el handle
dinit, pared separada e infranqueable sin protocolo).

## Estado contenedor-como-root (docker local = CI, 6 Running)
crond, dbus-daemon, sshd, tmpfiles-clean, udevd, uuidd. Completados
toda la rama sana. chronyd: pared shell+timeout (arriba). Frontera:
devmon (protocolo), mounts, resto en cascada correcta.

## Regla que sale de aquí para N > 9
El traductor no conoce la shell destino: emite el puente igual (es lo
correcto donde la shell acompaña) y lo documenta. No se reescriben
args (-N 11): cirugía específica de servicio, prohibida por minimalismo.
