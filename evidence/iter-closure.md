# Iteración: arranque perezoso (boot closure → enabled=no)

## Hallazgo que lo exige (dinit real privilegiado, dinitctl list)
dinit SOLO arranca el cierre de boot: sshd/uuidd/crond ni siquiera
aparecen en su lista (sin boot.d, nada los arrastra: jamás cargan).
El e2e celebraba "sshd Running" — DIVERGENTE: ssh encendido por
defecto donde dinit lo deja apagado. Riesgo de seguridad, no feature.

## Implementación (~25 líneas, BFS en shell)
Pulls = required + waits + .d (todos arrancan sus deps por manual);
after/before nunca arrastran. BFS desde boot sobre listas pull/N;
fuera del cierre => `enabled = no` anexado (operador arranca explícito,
igual que `dinitctl enable/start`). Sin boot => aviso, todo enabled.
Nombres raros en deps no atraviesan dirs (solo lecturas, jamás
escrituras con $d). Fixtures: lonely+mysql fuera, sshd/boot dentro.

## Resultado corpus real: 8 fuera (crond, device, recovery, single,
## ssh-keygen, sshd, uuidd, zram-device), 55 dentro. Ningún iniciado
## por dinit queda fuera (comparado contra su lista).
