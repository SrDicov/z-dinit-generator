# z-dinit-generator

Traduce descripciones dinit a descripciones zinit (C1). Texto dentro,
texto fuera; no arranca nada.

```sh
./dinit-import.sh /etc/dinit.d ./services.d
zinit check ./services.d
```
