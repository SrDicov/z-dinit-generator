# Iteración: orden before/after medido en runtime (no en papel)

## Experimento marcador (jaula local, reload-all, 3 servicios nuevos)
`ord-a --before--> ord-b --before--> ord-c`, cada uno `sleep 2` y
marca temporal en order.log. Traducido: opt:-aristas en ord-b/c.
Resultado:
a-1791745018.103977235
c-1791745018.106153068
b-1791745018.104921947
Las tres marcas caen en ~2 ms: el reconciliador los lanza en la misma
pasada. opt: NO serializa el arranque (solo desempata el orden del
plan). before/after quedan como hint de plan, sin espera runtime.

## Por qué opt: sigue siendo el vecino correcto (y no required)
dinit before = espera condicional + sin pull + sin cascada. C1 no tiene
esa arista. required la clava en orden pero añade pull+cascada: probado
que aparca pre-local tras uuidd donde dinit avanzaría. opt: nunca
bloquea: el boot avanza en degradado y en sano el orden se intenta.
El resto lo absorbe el modelo de reinicios (un dependiente que arranca
antes de su dependencia lista reintenta; dinit esperaría). Divergencia
conocida, dirección segura, sin fix posible en C1 congelado.

## Nota de test
Comandos con comillas/expansiones NO valen en `type = process` (ni en
dinit, que trocea por blancos, ni en zinit): el primer intento con
`/bin/sh -c '...$(date)...'` murió en el split. El test usa helper en
/usr/local/bin + args planos, patrón válido en ambos mundos.
