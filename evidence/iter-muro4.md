# Iteración: MURO 4 (stop-command + chain-to) con portadores en runtime

## stop-command => trap TERM (proceso). Carrier `graceful` en jaula:
`stop` → marcador `stopped` escrito → Stopped limpio, desired down,
restarts 0/3, sin KILL. Sin trap habría muerto sin limpiar.
Tipo fuerza script; oneshot/forking lo conservan directo con aviso
(oneshot rompería el split, forking no saldría a adoptar).

## chain-to => `zctl start` sin espera (solo proceso). Carriers:
- chainA(true) dispara, chainB (enabled=no escrito a mano) corre y
  marca `chained`: el mecanismo funciona y B no corre sin disparo.
- failerA(false) NO dispara: chainC sigue down, sin marca BAD.
- oneshot+chain se AVISA y se suelta: el wrap /bin/sh -c se rompe en
  el split ingenuo (probado: loop exit-2). Inversión a depends
  RECHAZADA con prueba: single(rescate, quizá nunca corre)->boot
  colgaría el arranque normal. B intacto en carga, siempre.

## Fixtures CI: graceful/chainer/ochain (texto + warns).
c1 del ci sube a v0.0.2 (tenía v0.0.1; el bug de orden ya no aplica).
