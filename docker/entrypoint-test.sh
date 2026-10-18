#!/usr/bin/env bash
# Test the logic of entrypoint.sh without a container
#
#   docker/entrypoint-test.sh
#
# `id`, `chown`, `setpriv` and `hitomi-server` are replaced by stand-ins that write down what
# they were called with, so what the script decides can be checked: which user it asks for,
# what it hands over, which commands it runs as the server, and what it refuses. It runs under
# `sh`, which is `dash` on Debian, the shell of the image.
set -uo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
STUBS="$WORK/stubs"
LOG="$WORK/log"
mkdir -p "$STUBS"

cat > "$STUBS/id" << 'EOF'
#!/bin/sh
echo "${FAKE_UID:-0}"
EOF
cat > "$STUBS/chown" << 'EOF'
#!/bin/sh
echo "chown $*" >> "$LOG"
[ -z "${FAKE_CHOWN_FAILS:-}" ]
EOF
# setpriv stands for what it does: runs the command that follows its options
cat > "$STUBS/setpriv" << 'EOF'
#!/bin/sh
echo "setpriv $*" >> "$LOG"
while [ $# -gt 0 ]; do
    case "$1" in --*) shift ;; *) break ;; esac
done
exec "$@"
EOF
cat > "$STUBS/hitomi-server" << 'EOF'
#!/bin/sh
echo "hitomi-server $* | umask $(umask) | HOME ${HOME:-unset}" >> "$LOG"
EOF
chmod +x "$STUBS"/*

failures=0
checks=0

# run <environment assignments...> -- <arguments of the entrypoint>
# sets OUTPUT (what the script printed), EXIT_CODE, and LOGGED (what the stand-ins saw)
run() {
    local env_args=()
    while [ "$1" != -- ]; do
        env_args+=("$1")
        shift
    done
    shift
    : > "$LOG"
    OUTPUT=$(env -i PATH="$STUBS:/usr/bin:/bin" LOG="$LOG" HITOMI_CONFIG_DIR="$WORK/config" "${env_args[@]}" sh "$HERE/entrypoint.sh" "$@" 2>&1)
    EXIT_CODE=$?
    LOGGED=$(cat "$LOG")
}

check() {
    checks=$((checks + 1))
    if [ "$2" = "$3" ]; then
        echo "ok    $1"
    else
        failures=$((failures + 1))
        echo "FAIL  $1" >&2
        echo "      expected: $2" >&2
        echo "      got:      $3" >&2
    fi
}

check_has() {
    checks=$((checks + 1))
    case "$3" in
        *"$2"*) echo "ok    $1" ;;
        *)
            failures=$((failures + 1))
            echo "FAIL  $1" >&2
            echo "      no '$2' in: $3" >&2
            ;;
    esac
}

check_lacks() {
    checks=$((checks + 1))
    case "$3" in
        *"$2"*)
            failures=$((failures + 1))
            echo "FAIL  $1" >&2
            echo "      unexpected '$2' in: $3" >&2
            ;;
        *) echo "ok    $1" ;;
    esac
}

echo "--- as root, the defaults"
run -- hitomi-server
check "the server runs" 0 "$EXIT_CODE"
check_has "only the config directory is handed over, recursively, to 1000:1000" "chown -R 1000:1000 $WORK/config" "$LOGGED"
check "nothing else is chowned" 1 "$(grep -c '^chown' "$LOG")"
check_has "root is given up for the user, with no groups and no new privileges" "setpriv --reuid=1000 --regid=1000 --clear-groups --no-new-privs hitomi-server" "$LOGGED"
check_has "the default umask is 022" "umask 0022" "$LOGGED"
check_has "HOME is the config directory, so nothing is written to /" "HOME $WORK/config" "$LOGGED"
check "the config directory is made" yes "$([ -d "$WORK/config" ] && echo yes || echo no)"

echo "--- PUID, PGID and UMASK"
run PUID=1234 PGID=4321 UMASK=027 -- hitomi-server
check_has "the user and group asked for" "setpriv --reuid=1234 --regid=4321" "$LOGGED"
check_has "chown to them" "chown -R 1234:4321" "$LOGGED"
check_has "UMASK applies to what the server writes" "umask 0027" "$LOGGED"
run PUID=99 PGID=100 UMASK=0077 -- hitomi-server
check_has "a four digit UMASK works too" "umask 0077" "$LOGGED"
run PUID=1234 PGID=4321 -- hitomi-server --some-flag
check_has "arguments reach the server" "hitomi-server --some-flag" "$LOGGED"

echo "--- the library and the export directory are never touched"
run PUID=1234 PGID=4321 HITOMI_LIBRARY_DIR=/data/library HITOMI_EXPORT_DIR=/data/export -- hitomi-server
check_lacks "no chown of the library" "/data/library" "$LOGGED"
check_lacks "no chown of the export directory" "/data/export" "$LOGGED"

echo "--- commands that need nothing prepared"
for command in hash-password healthcheck; do
    rm -rf "$WORK/config"
    run PUID=1234 PGID=4321 -- "$command"
    check_has "$command runs the server with it, as the user" "setpriv --reuid=1234 --regid=4321 --clear-groups --no-new-privs hitomi-server $command" "$LOGGED"
    check_lacks "$command hands nothing over" "chown" "$LOGGED"
    check "$command makes no config directory" no "$([ -d "$WORK/config" ] && echo yes || echo no)"
done

echo "--- started with user: or --user, so not as root"
run FAKE_UID=1000 PUID=2000 PGID=2000 -- hitomi-server
check "the server runs" 0 "$EXIT_CODE"
check_lacks "nothing is chowned, a user that is not root could not" "chown" "$LOGGED"
check_lacks "and no user is switched to" "setpriv" "$LOGGED"
check_has "the umask still applies" "umask 0022" "$LOGGED"

echo "--- PUID 0 is root, with a warning"
run PUID=0 PGID=0 -- hitomi-server
check_has "it warns" "running as root" "$OUTPUT"
check_lacks "and does not switch user" "setpriv" "$LOGGED"
check_lacks "or chown" "chown" "$LOGGED"
check_has "the server still runs" "hitomi-server" "$LOGGED"

echo "--- a config directory that can not be handed over is a warning"
run FAKE_CHOWN_FAILS=1 PUID=1234 PGID=4321 -- hitomi-server
check_has "it says so" "could not hand" "$OUTPUT"
check_has "and the server starts, where it says what is wrong" "setpriv --reuid=1234" "$LOGGED"

echo "--- what is refused"
for bad in abc -1 "1 2" " " 1.5 "1;id"; do
    run "PUID=$bad" -- hitomi-server
    check "PUID '$bad' stops the start" 1 "$EXIT_CODE"
    check_has "and says PUID has to be a number" "PUID has to be a number" "$OUTPUT"
    check "and runs nothing" "" "$LOGGED"
done
run PGID=abc -- hitomi-server
check_has "a PGID that is no number" "PGID has to be a number" "$OUTPUT"
for bad in 999 08 abc 0222222 -1 " "; do
    run "UMASK=$bad" -- hitomi-server
    check "UMASK '$bad' stops the start" 1 "$EXIT_CODE"
    check_has "and says it has to be octal" "UMASK has to be an octal number" "$OUTPUT"
done

echo "--- the time zone"
if [ -e /usr/share/zoneinfo/Europe/Berlin ]; then
    run TZ=Europe/Berlin -- hitomi-server
    check_lacks "a known time zone is not warned about" "unknown" "$OUTPUT"
fi
run TZ=Nowhere/Land -- hitomi-server
check_has "an unknown one is, and the server still starts" "unknown" "$OUTPUT"
check_has "it starts" "hitomi-server" "$LOGGED"

echo
echo "$((checks - failures)) of $checks checks passed"
[ "$failures" = 0 ]
