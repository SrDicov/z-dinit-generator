#!/bin/sh
# dinit-import.sh <dinitdir> <outdir>: dinit -> zinit, texto a texto.
# depends-ms/waits-for/after=>opt:; bgprocess sin pid=>process;
# internal/triggered=>oneshot true (target no admite depends, E011);
# restart bool=>always/never; resto se avisa y se omite.
# Salida: 0 (saltos son avisos), 1 fallo E/S, 2 uso. POSIX sh.
set -eu
p=dinit-import
warn() { printf '%s: %s\n' "$p" "$*" >&2; }
[ $# -eq 2 ] || { printf 'usage: %s <dinitdir> <outdir>\n' "$p" >&2; exit 2; }
in=$1 out=$2
[ -d "$in" ] || { warn "$in: not a directory"; exit 1; }
mkdir -p "$out" || exit 1
squote() { printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"; }
t() { printf '%s' "$1" | sed 's/^ *//;s/ *$//'; }
dur() { case "$1" in *[!0-9]*) printf '%s' "$1";; *) printf '%ss' "$1";; esac; }
convert() {
    f=$1 n=$2
    type= command= deps= restart= stop= start= user= wdir= log= pid=
    tmp=${TMPDIR:-/tmp}/$p.$$
    sed -e 's/#.*$//' -e '/^ *$/d' "$f" >"$tmp" || return 1
    while IFS= read -r ln; do
        case "$ln" in *"+="*) k=${ln%%+=*}; v=${ln#*+=}; ap=1;; *"="*) k=${ln%%=*}; v=${ln#*=}; ap=0;; *":"*) k=${ln%%:*}; v=${ln#*:}; ap=0;; *) warn "$n: bad line, ignored"; continue;; esac
        k=$(t "$k"); v=$(t "$v")
        case "$k" in
            type) type=$v;;
            command) if [ "$ap" = 1 ]; then command="$command $v"; else command=$v; fi;;
            depends-on) deps=${deps:+"$deps, "}$v;;
            depends-ms|waits-for|after) deps=${deps:+"$deps, "}opt:$v;;
            restart) case "$v" in true|yes|1) restart=always;; false|no|0) restart=never;; *) warn "$n: restart=$v ignored";; esac;;
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
    if [ -n "$wdir" ]; then command="cd $(squote "$wdir") && $command"; fi
    {
        printf 'type = %s\n' "$zt"
        printf 'command = %s\n' "$command"
        [ -z "$deps" ] || printf 'depends = %s\n' "$deps"
        [ -z "$restart" ] || printf 'restart = %s\n' "$restart"
        [ -z "$stop" ] || printf 'stop-timeout = %s\n' "$stop"
        [ -z "$start" ] || printf 'start-timeout = %s\n' "$start"
        [ -z "$user" ] || printf 'user = %s\n' "$user"
        [ -z "$log" ] || printf 'log = %s\n' "$log"
        [ "$zt" = forking ] && [ -n "$pid" ] && printf 'pid-file = %s\n' "$pid"
        :
    } >"$out/$n.conf" || { warn "cannot write $out/$n.conf"; return 1; }
}
for f in "$in"/*; do
    [ -e "$f" ] || continue
    [ -f "$f" ] || continue
    n=$(basename "$f")
    case "$n" in *[!a-zA-Z0-9_.@+-]*|""|.*|@*|*@|*..*|*@*@*) warn "$n: odd name, skipped"; continue;; esac
    convert "$f" "$n" || exit 1
done
