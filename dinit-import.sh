#!/bin/sh
# dinit-import.sh <dinitdir> <outdir>: dinit -> zinit, texto a texto.
# Motor heredado, comportamiento congelado: `z-dinit-generator convert` es
# lo autoritativo (paridad byte-identica en CI sobre el corpus simple).
# Arranque perezoso como dinit: solo boot y lo que arrastra (required +
# waits + .d; after/before nunca arrastran) arranca solo; el resto sale
# con `enabled = no` para arranque explícito del operador.
# depends-on/ms=>required (ms pierde el corte post-arranque);
# waits-for/after=>opt: (ordena sin bloquear); before X=>opt: en X;
# bgprocess sin pid=>process; internal/triggered=>oneshot true (E011);
# ready-notification=>notify+preambulo fd, sin ella=>none (dinit
# arranca en exec; el defecto notify colaria 30 s por demonio);
# restart ausente/true en proceso=>always+`3 restarts per 10s`
# (envolvente dinit: el always solo de zinit es infinito);
# on-failure=>igual salvo oneshot=>never; sin logfile=>none;
# stop-command=>trap TERM (solo proceso; oneshot/forking avisado);
# chain-to=>zctl start sin espera (solo proceso; B intacto para que un
# rescate nunca bloquee el arranque); *-d depends=>required, *-d
# waits/ms=>opt:; resto se avisa y se omite.
# Salida: 0 (saltos son avisos), 1 fallo E/S, 2 uso. POSIX sh.
set -eu
p=dinit-import
warn() { printf '%s: %s\n' "$p" "$*" >&2; }
[ $# -eq 2 ] || { printf 'usage: %s <dinitdir> <outdir>\n' "$p" >&2; exit 2; }
in=$1 out=$2
[ -d "$in" ] || { warn "$in: not a directory"; exit 1; }
mkdir -p "$out" || exit 1
bef=${TMPDIR:-/tmp}/$p.before.$$
: >"$bef" || exit 1
pulldir=${TMPDIR:-/tmp}/$p.pulls.$$
mkdir -p "$pulldir" || exit 1
pull() { printf '%s\n' "$2" >>"$pulldir/$1" || return 1; }
squote() { printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"; }
t() { printf '%s' "$1" | sed 's/^ *//;s/ *$//'; }
dur() { case "$1" in *[!0-9]*) printf '%s' "$1";; *) printf '%ss' "$1";; esac; }
convert() {
    f=$1 n=$2
    type= command= deps= restart= budget= stop= start= user= wdir= log= pid= ready= notifyfd=
    stopcmd= chain= chainalways=
    tmp=${TMPDIR:-/tmp}/$p.$$
    sed -e 's/#.*$//' -e '/^ *$/d' "$f" >"$tmp" || return 1
    while IFS= read -r ln; do
        case "$ln" in *"+="*) k=${ln%%+=*}; v=${ln#*+=}; ap=1;; *"="*) k=${ln%%=*}; v=${ln#*=}; ap=0;; *":"*) k=${ln%%:*}; v=${ln#*:}; ap=0;; *) warn "$n: bad line, ignored"; continue;; esac
        k=$(t "$k"); v=$(t "$v")
        case "$k" in
            type) type=$v;;
            command) if [ "$ap" = 1 ]; then command="$command $v"; else command=$v; fi;;
            depends-on|depends-ms) deps=${deps:+"$deps, "}$v; pull "$n" "$v" || return 1;;
            waits-for) deps=${deps:+"$deps, "}opt:$v; pull "$n" "$v" || return 1;;
            after) deps=${deps:+"$deps, "}opt:$v;;
            before) printf '%s %s\n' "$v" "opt:$n" >>"$bef" || return 1;;
            depends-on.d|depends-ms.d|waits-for.d)
                case "$k" in depends-on.d) pre=;; *) pre=opt:;; esac
                dd=$v; bn=$(basename "$dd")
                if [ -d "$in/$bn" ]; then dd=$in/$bn; fi
                if [ -d "$dd" ]; then for e in "$dd"/*; do
                    [ -e "$e" ] || continue
                    deps=${deps:+"$deps, "}$pre$(basename "$e")
                    pull "$n" "$(basename "$e")" || return 1
                done; else warn "$n: $k $v missing, ignored"; fi;;
            ready-notification)
                ready=notify
                case "$v" in pipefd:*) nfd=${v#pipefd:};
                    case "$nfd" in ''|*[!0-9]*) warn "$n: ready $v ignored";; *) notifyfd=$nfd;; esac;;
                *) warn "$n: ready $v ignored";; esac;;
            restart) case "$v" in true|yes|1) restart=always;; false|no|0) restart=never;; on-failure) restart=on-failure;; *) warn "$n: restart=$v ignored";; esac;;
            restart-delay|restart-limit-*) warn "$n: $k ignored; 3-per-10s envelope used";;
            stop-command) if [ "$ap" = 1 ]; then stopcmd="$stopcmd $v"; else stopcmd=$v; fi;;
            chain-to) chain=$v;;
            options) case "$v" in *always-chain*) chainalways=1;; esac
                warn "$n: options ignored except always-chain";;
            stop-timeout) stop=$(dur "$v");;
            start-timeout) start=$(dur "$v");;
            run-as) user=$v;;
            working-dir) wdir=$v;;
            logfile) log=file:$v;;
            pid-file) pid=$v;;
            *) warn "$n: $k ignored";;
        esac
    done <"$tmp"
    rm -f "$tmp"
    case "$type" in
        process) zt=process;;
        bgprocess) if [ -n "$pid" ]; then zt=forking; else zt=process; warn "$n: bgprocess without pid-file as process"; fi;;
        scripted) zt=oneshot;;
        internal|triggered) zt=oneshot; [ -n "$command" ] || command=true;;
        *) warn "$n: type '$type' unknown, skipped"; return 0;;
    esac
    if [ -z "$command" ]; then warn "$n: no command, skipped"; return 0; fi
    # Shell assembly: cd, fd-dup, TERM-trap and chain-tail compose as one
    # prelude. dinit runs stop-command INSTEAD of signalling; the trap
    # reproduces that (group TERM kills the child too, cleanup runs
    # post-death like the early clock/rng scripts want). chain-to fires
    # `zctl start` on clean exit with no wait (a rescue chain must never
    # gate boot, so B is left untouched at load). Plain services keep
    # direct exec (no shell tax, pid = service); composed ones become
    # script, whose whole line reaches /bin/sh -c unsplit. oneshot and
    # forking stay direct no matter what: oneshot would shred a prelude
    # in the naive word split (proven: exit-2 loop), and a shell around
    # forking would never exit to adopt. Dropped features are warned.
    pre=""
    [ -n "$wdir" ] && pre="cd $(squote "$wdir") && "
    if [ -n "$notifyfd" ]; then
        if [ "$zt" = oneshot ] || [ "$zt" = forking ]; then warn "$n: pipefd needs a plain process, ignored"; notifyfd=;
        else pre="${pre}exec $notifyfd>&\$ZINIT_NOTIFY_FD; "; fi
    fi
    stunt=""
    [ -n "$stopcmd" ] && stunt="trap $(squote "$stopcmd") TERM; "
    tail=""
    if [ -n "$chain" ]; then
        case "$chain" in *[!a-zA-Z0-9_.@+-]*|""|.*|@*|*@|*..*|*@*@*) warn "$n: chain-to $chain ignored"; chain=;; esac
    fi
    if [ -n "$chain" ]; then
        if [ -n "$chainalways" ]; then tail="; code=\$?; command -v zctl >/dev/null 2>&1 && zctl start $chain; exit \$code";
        else tail="; code=\$?; if [ \$code -eq 0 ]; then command -v zctl >/dev/null 2>&1 && zctl start $chain; fi; exit \$code"; fi
    fi
    if [ -n "$pre$stunt$tail" ]; then
        if [ "$zt" = forking ] || [ "$zt" = oneshot ]; then
            [ -z "$wdir" ] || warn "$n: working-dir needs a shell, ignored"
            [ -z "$stopcmd" ] || warn "$n: stop-command needs a shell, ignored"
            [ -z "$chain" ] || warn "$n: chain-to needs a shell, ignored"
        else
            zt=script
            if [ -n "$stunt$tail" ]; then command="$stunt$pre$command$tail";
            else command="${pre}exec $command"; fi
        fi
    fi
    case "$zt" in
        oneshot) [ "$restart" = on-failure ] && restart=never;;
    esac
    # dinit restarts by default, bounded 3-per-10s. zinit always is
    # unbounded (devmon spun 316x), so every always carries the dinit
    # envelope explicitly. Absent restart on process/bgprocess means the
    # dinit default: always, bounded. (tmpfiles-clean proved clean exits
    # must re-arm: it sleeps 900 s, cleans, exits 0, repeats.)
    if [ "$restart" = always ]; then
        budget='3 restarts per 10s'
    else
        case "$zt" in
            process|forking|script) [ -n "$restart" ] || { restart=always; budget='3 restarts per 10s'; };;
        esac
    fi
    [ -n "$log" ] || log=none
    # dinit without ready-notification starts at exec; zinit default is
    # notify (30 s artificial delay per daemon), so spell none out.
    [ -n "$ready" ] || ready=none
    {
        printf 'type = %s\n' "$zt"
        printf 'command = %s\n' "$command"
        [ -z "$deps" ] || printf 'depends = %s\n' "$deps"
        [ -z "$restart" ] || printf 'restart = %s\n' "$restart"
        [ -z "$budget" ] || printf 'restart-budget = %s\n' "$budget"
        [ -z "$stop" ] || printf 'stop-timeout = %s\n' "$stop"
        [ -z "$start" ] || printf 'start-timeout = %s\n' "$start"
        [ -z "$user" ] || printf 'user = %s\n' "$user"
        [ -z "$log" ] || printf 'log = %s\n' "$log"
        [ -z "$ready" ] || printf 'ready = %s\n' "$ready"
        [ "$zt" = forking ] && [ -n "$pid" ] && printf 'pid-file = %s\n' "$pid"
        :
    } >"$out/$n.conf" || { warn "cannot write $out/$n.conf"; return 1; }
}
add_req() {
    [ -f "$out/$1.conf" ] || { warn "$2: before $1 unknown, ignored"; return 0; }
    pw=${TMPDIR:-/tmp}/$p.patch.$$
    hit=0
    while IFS= read -r l; do
        case "$l" in "depends = "*) l="depends = $2, ${l#depends = }"; hit=1;; esac
        printf '%s\n' "$l"
    done <"$out/$1.conf" >"$pw" || return 1
    [ "$hit" = 1 ] || printf 'depends = %s\n' "$2" >>"$pw" || return 1
    mv "$pw" "$out/$1.conf" || return 1
}
for f in "$in"/*; do
    [ -e "$f" ] || continue
    [ -f "$f" ] || continue
    n=$(basename "$f")
    case "$n" in *[!a-zA-Z0-9_.@+-]*|""|.*|@*|*@|*..*|*@*@*) warn "$n: odd name, skipped"; continue;; esac
    convert "$f" "$n" || exit 1
done
while read -r b a; do [ -n "${b:-}" ] && [ -n "${a:-}" ] && add_req "$b" "$a" || exit 1; done <"$bef"
rm -f "$bef"
# Boot closure: dinit starts lazily (boot + what it pulls), never the
# whole directory. Services outside the closure boot disabled, like
# un-enabled dinit services: the operator starts them explicitly.
# Pulls = required + waits + .d (all start their deps); after/before
# never pull. BFS from boot over recorded pull lists.
if [ -f "$in/boot" ]; then
    work=${TMPDIR:-/tmp}/$p.work.$$ closed=${TMPDIR:-/tmp}/$p.closed.$$
    printf 'boot\n' >"$work" || exit 1
    : >"$closed" || exit 1
    while IFS= read -r s || [ -n "$s" ]; do
        case "$s" in ""|"#"*) continue;; esac
        grep -qxF "$s" "$closed" 2>/dev/null && continue
        printf '%s\n' "$s" >>"$closed" || exit 1
        [ -f "$pulldir/$s" ] && while IFS= read -r d || [ -n "$d" ]; do
            case "$d" in ""|"#"*) continue;; esac
            grep -qxF "$d" "$closed" 2>/dev/null || printf '%s\n' "$d" >>"$work" || exit 1
        done <"$pulldir/$s"
    done <"$work"
    for f in "$out"/*.conf; do
        [ -e "$f" ] || continue
        n=$(basename "$f" .conf)
        grep -qxF "$n" "$closed" 2>/dev/null || printf 'enabled = no\n' >>"$f" || exit 1
    done
    rm -f "$work" "$closed"
else
    warn "no boot service; everything enabled"
fi
rm -rf "$pulldir"
