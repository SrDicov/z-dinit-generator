#!/bin/sh
# test-dinitctl.sh — shim mapping against a stub zctl, no supervisor needed.
set -eu
d=$(mktemp -d); trap 'rm -rf "$d"' EXIT
cat >"$d/zctl" <<'EOF'
#!/bin/sh
case "$1" in
    status) case "$2" in
        running-svc) echo "R t 0 running-svc running desired=up pid=9 up=3 restarts=0/5";;
        dead-svc) echo "R t 0 dead-svc stopped desired=up pid=- up=- restarts=5/5";;
        down-svc) echo "R t 0 down-svc stopped desired=down pid=- up=- restarts=0/5";;
        *) echo "E t 3 unknown service"; exit 1;;
    esac;;
    list|start|stop|restart) echo "R t 0 ok";;
    *) echo "E t 2 bad verb"; exit 1;;
esac
EOF
chmod +x "$d/zctl"
export PATH="$d:$PATH"
S=$(dirname "$0")/../dinitctl
n=0
t() {
    desc=$1; want=$2; shift 2
    n=$((n + 1))
    if out=$("$@" 2>"$d/err"); then rc=0; else rc=$?; fi
    [ "$rc" = "$want" ] || { echo "FAIL[$n] $desc: rc=$rc want=$want: $out $(cat "$d/err")"; exit 1; }
}
t setenv-ok 0 sh "$S" setenv FOO=1
grep -q 'no activation environment' "$d/err" || { echo "FAIL setenv must announce"; exit 1; }
t unsetenv-ok 0 sh "$S" unsetenv FOO
t flag-stripped 0 sh "$S" --use-passed-cfd setenv 'A=b c'
t unknown-cmd 1 sh "$S" frobnicate
t no-args 1 sh "$S"
t trigger-loud 1 sh "$S" trigger foo
t wake-loud 1 sh "$S" wake foo
t status-pass 0 sh "$S" status running-svc
t start-pass 0 sh "$S" start foo
t is-started-run 0 sh "$S" is-started running-svc
t is-started-dead 1 sh "$S" is-started dead-svc
t is-started-down 1 sh "$S" is-started down-svc
t is-started-missing 1 sh "$S" is-started nosuch
t is-failed-dead 0 sh "$S" is-failed dead-svc
t is-failed-run 1 sh "$S" is-failed running-svc
t is-failed-down 1 sh "$S" is-failed down-svc
echo "dinitctl: $n mapping checks ok"
