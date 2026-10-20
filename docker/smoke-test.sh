#!/usr/bin/env bash
# Start the image and check what it promises
#
#   IMAGE=hitomi-downloader docker/smoke-test.sh
#
# Environment:
#   IMAGE                 the image to test (required)
#   DOCKER                docker or podman, default docker
#   PLATFORM              e.g. linux/arm64, to test an image of another platform under emulation
#   EXPECT_VERSION        the version /api/health has to report, e.g. 1.2.3
#   HOST_NETWORK=1        run with the network of the host instead of publishing a port
#   REQUIRE_PUID_TESTS=1  fail instead of skip when the container can not switch to another user
#   REQUIRE_HEALTH_STATUS=1  also wait for the status of the HEALTHCHECK instruction, which
#                         needs a container engine that runs healthchecks (Docker does)
set -uo pipefail

IMAGE=${IMAGE:?set IMAGE to the image to test}
DOCKER=${DOCKER:-docker}
platform=()
[ -n "${PLATFORM:-}" ] && platform=(--platform "$PLATFORM")

WORK=$(mktemp -d)
# names are unique per run, so a container a former run left behind can not be in the way
RUN_ID=smoke-$$
CONTAINERS=()
VOLUMES=()
failures=0
checks=0

cleanup() {
    for name in "${CONTAINERS[@]:-}"; do
        [ -n "$name" ] && "$DOCKER" rm -f "$name" > /dev/null 2>&1
    done
    for volume in "${VOLUMES[@]:-}"; do
        [ -n "$volume" ] && "$DOCKER" volume rm -f "$volume" > /dev/null 2>&1
    done
    chmod -R u+rwX "$WORK" 2> /dev/null
    rm -rf "$WORK"
}
trap cleanup EXIT

ok() {
    checks=$((checks + 1))
    echo "ok    $1"
}

fail() {
    checks=$((checks + 1))
    failures=$((failures + 1))
    echo "FAIL  $1" >&2
    [ -n "${2:-}" ] && echo "      $2" | head -c 600 >&2 && echo >&2
}

# check <description> <expected> <actual>
check_equals() {
    if [ "$2" = "$3" ]; then ok "$1"; else fail "$1" "expected '$2', got '$3'"; fi
}

# check_contains <description> <needle> <haystack>
check_contains() {
    case "$3" in *"$2"*) ok "$1" ;; *) fail "$1" "no '$2' in: $3" ;; esac
}

# Can the container give up root? Not where the user namespace has a single id, as in rootless
# Podman, which is only a limit of where this runs. There the container stays root.
can_switch_user=1
root_env=()
if ! "$DOCKER" run --rm "${platform[@]}" --entrypoint setpriv "$IMAGE" --reuid=1234 --regid=4321 --clear-groups id -u > /dev/null 2>&1; then
    can_switch_user=0
    root_env=(-e PUID=0 -e PGID=0)
    if [ -n "${REQUIRE_PUID_TESTS:-}" ]; then
        fail "the container can switch to the user PUID:PGID"
    else
        echo "skip  the container can not switch users here, it runs as root and the PUID tests are skipped"
    fi
fi

PASSWORD='smoke test password'
HASH=$(printf '%s\n' "$PASSWORD" | "$DOCKER" run --rm -i "${platform[@]}" "${root_env[@]}" "$IMAGE" hash-password 2> /dev/null)
# shellcheck disable=SC2016  # the pattern is the literal start of the hash
case "$HASH" in
    '$argon2id$'*) ok "hash-password prints an Argon2 hash" ;;
    *) fail "hash-password prints an Argon2 hash" "$HASH"; exit 1 ;;
esac

free_port() {
    python3 -I -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])'
}

# start <name> [docker run arguments...]  -> sets BASE to the URL of the server
start() {
    local name=$1
    shift
    CONTAINERS+=("$name")
    if [ -n "${HOST_NETWORK:-}" ]; then
        local port
        port=$(free_port)
        "$DOCKER" run -d --name "$name" "${platform[@]}" --network=host -e HITOMI_PORT="$port" "$@" "$IMAGE" > /dev/null
        BASE="http://127.0.0.1:$port"
    else
        "$DOCKER" run -d --name "$name" "${platform[@]}" -p 127.0.0.1::8080 "$@" "$IMAGE" > /dev/null
        BASE="http://$("$DOCKER" port "$name" 8080/tcp | head -n 1)"
    fi
}

# Wait until the server answers its own healthcheck, or the container is gone
wait_healthy() {
    local name=$1 tries=${2:-90}
    for _ in $(seq "$tries"); do
        if "$DOCKER" exec "$name" hitomi-server healthcheck > /dev/null 2>&1; then return 0; fi
        [ "$("$DOCKER" inspect --format '{{.State.Running}}' "$name" 2> /dev/null)" = true ] || return 1
        sleep 1
    done
    return 1
}

# run_expecting_failure <name> <docker run arguments...>  -> sets OUTPUT and EXIT_CODE
run_expecting_failure() {
    local name=$1
    shift
    CONTAINERS+=("$name")
    OUTPUT=$("$DOCKER" run --name "$name" "${platform[@]}" "$@" "$IMAGE" 2>&1)
    EXIT_CODE=$?
}

login_cookie() {
    curl -s -c "$WORK/jar-$1" -X POST -H 'content-type: application/json' \
        "$BASE/auth/login" -d "{\"username\":\"smoke\",\"password\":\"$PASSWORD\"}" -o /dev/null -w '%{http_code}'
}

auth_env=(-e HITOMI_USERNAME=smoke -e "HITOMI_PASSWORD_HASH=$HASH")

echo "--- the image has no desktop libraries"
packages=$("$DOCKER" run --rm "${platform[@]}" --entrypoint sh "$IMAGE" -c "dpkg -l | grep -Ei 'gtk|webkit|soup' | wc -l")
check_equals "no GTK, WebKit or libsoup package in the image" 0 "$packages"
linked=$("$DOCKER" run --rm "${platform[@]}" --entrypoint sh "$IMAGE" -c "ldd /usr/local/bin/hitomi-server | grep -Ei 'gtk|webkit|soup' | wc -l")
check_equals "the server links none of them" 0 "$linked"

echo "--- a normal start"
mkdir -p "$WORK/library" "$WORK/export" "$WORK/config"
chmod 0777 "$WORK/library" "$WORK/export" "$WORK/config"
puid=0 pgid=0 umask_value=027
if [ "$can_switch_user" = 1 ]; then puid=1234 pgid=4321; fi
library_owner_before=$(stat -c '%u:%g %a' "$WORK/library")
start "$RUN_ID-main" "${auth_env[@]}" -e PUID=$puid -e PGID=$pgid -e UMASK=$umask_value -e TZ=Europe/Berlin \
    -v "$WORK/library:/data/library" -v "$WORK/export:/data/export" -v "$WORK/config:/data/config"
if wait_healthy "$RUN_ID-main"; then ok "the server starts and answers its healthcheck"; else
    fail "the server starts and answers its healthcheck" "$("$DOCKER" logs "$RUN_ID-main" 2>&1 | tail -5)"
    exit 1
fi

if [ -n "${REQUIRE_HEALTH_STATUS:-}" ]; then
    status=starting
    for _ in $(seq 60); do
        status=$("$DOCKER" inspect --format '{{.State.Health.Status}}' "$RUN_ID-main")
        [ "$status" = healthy ] && break
        sleep 2
    done
    check_equals "the HEALTHCHECK of the image turns healthy" healthy "$status"
fi

health=$(curl -s "$BASE/api/health")
check_contains "/api/health reports ok" '"status":"ok"' "$health"
if [ -n "${EXPECT_VERSION:-}" ]; then
    check_contains "/api/health reports the version of the release" "\"version\":\"$EXPECT_VERSION\"" "$health"
fi

check_equals "a page without a login redirects to the login" 303 "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/")"
check_equals "the API without a login answers 401" 401 "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/api/get_config")"
check_equals "a wrong password is refused" 401 "$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' "$BASE/auth/login" -d '{"username":"smoke","password":"wrong"}')"
check_equals "the right password logs in" 200 "$(login_cookie main)"
config=$(curl -s -b "$WORK/jar-main" -X POST "$BASE/api/get_config")
check_contains "the library is the directory mounted at the default" '"downloadDir":"/data/library"' "$config"
check_contains "the web frontend is served after the login" '<div id="app">' "$(curl -s -b "$WORK/jar-main" "$BASE/")"

check_equals "the server is the process the container runs, and runs as PUID" "$puid" \
    "$("$DOCKER" exec "$RUN_ID-main" sh -c "awk '/^Uid:/ {print \$2}' /proc/1/status")"
check_equals "its group is PGID" "$pgid" "$("$DOCKER" exec "$RUN_ID-main" sh -c "awk '/^Gid:/ {print \$2}' /proc/1/status")"
if [ "$can_switch_user" = 1 ]; then
    check_equals "config.json belongs to PUID:PGID" "$puid:$pgid" "$(stat -c '%u:%g' "$WORK/config/config.json" 2> /dev/null || echo missing)"
    check_equals "UMASK decides the rights of config.json (027 -> 640)" 640 "$(stat -c '%a' "$WORK/config/config.json" 2> /dev/null || echo missing)"
fi
check_equals "sessions.json is readable by its owner only" 600 "$(stat -c '%a' "$WORK/config/sessions.json" 2> /dev/null || echo missing)"
check_equals "the library keeps the owner and mode it had, it is never chowned" "$library_owner_before" "$(stat -c '%u:%g %a' "$WORK/library")"
check_equals "the server leaves nothing behind in the library" 0 "$(find "$WORK/library" -mindepth 1 | wc -l)"
zone=$("$DOCKER" exec "$RUN_ID-main" date +%Z)
case "$zone" in CET | CEST) ok "the time zone is applied (Europe/Berlin is $zone)" ;; *) fail "the time zone is applied" "Europe/Berlin should be CET or CEST, got '$zone'" ;; esac

echo "--- docker stop"
started=$(date +%s)
"$DOCKER" stop -t 20 "$RUN_ID-main" > /dev/null 2>&1
elapsed=$(($(date +%s) - started))
check_equals "the server exits with 0 when told to stop" 0 "$("$DOCKER" inspect --format '{{.State.ExitCode}}' "$RUN_ID-main")"
if [ "$elapsed" -lt 15 ]; then ok "it stops at once, not by being killed after the timeout (${elapsed}s)"; else fail "it stops at once" "took ${elapsed}s"; fi
check_contains "it says it is shutting down" "Shutting down" "$("$DOCKER" logs "$RUN_ID-main" 2>&1)"

echo "--- the paths are free to choose"
odd_library="$WORK/odd library ü"
odd_target="/mnt/Meine Bibliothek ü"
mkdir -p "$odd_library" "$WORK/config-odd" "$WORK/export-odd"
chmod 0777 "$odd_library" "$WORK/config-odd" "$WORK/export-odd"
start "$RUN_ID-odd" "${auth_env[@]}" -e PUID=$puid -e PGID=$pgid -e "HITOMI_LIBRARY_DIR=$odd_target" -e HITOMI_EXPORT_DIR=/somewhere/export \
    -v "$odd_library:$odd_target" -v "$WORK/config-odd:/data/config" -v "$WORK/export-odd:/somewhere/export"
if wait_healthy "$RUN_ID-odd"; then
    ok "a library with a space and an umlaut in its path, at a path the image does not know"
    login_cookie odd > /dev/null
    check_contains "the server uses it" "\"downloadDir\":\"$odd_target\"" "$(curl -s -b "$WORK/jar-odd" -X POST "$BASE/api/get_config")"
else
    fail "a library with a space and an umlaut in its path" "$("$DOCKER" logs "$RUN_ID-odd" 2>&1 | tail -5)"
fi
"$DOCKER" rm -f "$RUN_ID-odd" > /dev/null 2>&1

echo "--- named volumes, and no bind mount from the host at all"
VOLUMES+=("$RUN_ID-lib" "$RUN_ID-exp" "$RUN_ID-cfg")
start "$RUN_ID-named" "${auth_env[@]}" -e PUID=$puid -e PGID=$pgid \
    -v "$RUN_ID-lib:/data/library" -v "$RUN_ID-exp:/data/export" -v "$RUN_ID-cfg:/data/config"
if wait_healthy "$RUN_ID-named"; then
    ok "it starts with named volumes for the library, the exports and the config"
else
    fail "it starts with named volumes" "$("$DOCKER" logs "$RUN_ID-named" 2>&1 | tail -5)"
fi
"$DOCKER" rm -f "$RUN_ID-named" > /dev/null 2>&1

echo "--- started as another user with user: / --user"
mkdir -p "$WORK/user-config" "$WORK/user-library" "$WORK/user-export"
chmod 0777 "$WORK/user-config" "$WORK/user-library" "$WORK/user-export"
start "$RUN_ID-user" --user "$(id -u):$(id -g)" "${root_env[@]}" "${auth_env[@]}" \
    -v "$WORK/user-library:/data/library" -v "$WORK/user-export:/data/export" -v "$WORK/user-config:/data/config"
if wait_healthy "$RUN_ID-user"; then
    ok "it starts with --user and without root"
    check_equals "and runs as that user" "$(id -u)" "$("$DOCKER" exec "$RUN_ID-user" sh -c "awk '/^Uid:/ {print \$2}' /proc/1/status")"
else
    fail "it starts with --user and without root" "$("$DOCKER" logs "$RUN_ID-user" 2>&1 | tail -5)"
fi
"$DOCKER" rm -f "$RUN_ID-user" > /dev/null 2>&1

echo "--- a start that has to fail, and say why"
mkdir -p "$WORK/ro-library"
chmod 0555 "$WORK/ro-library"
# a user other than the owner, so the directory is closed to it. Root ignores permissions.
uid_for_ro=$puid
[ "$can_switch_user" = 1 ] || uid_for_ro=
if [ -n "$uid_for_ro" ]; then
    run_expecting_failure "$RUN_ID-ro" "${auth_env[@]}" -e PUID=$puid -e PGID=$pgid -v "$WORK/ro-library:/data/library" -v "$WORK/export:/data/export" -v "$WORK/config:/data/config"
    if [ "$EXIT_CODE" != 0 ]; then ok "a library the user can not write stops the start"; else fail "a library the user can not write stops the start" "$OUTPUT"; fi
    check_contains "and says it is not writable" "is not writable" "$OUTPUT"
    check_contains "and names the path" "/data/library" "$OUTPUT"
    check_contains "and the user and what to do about it" "PUID" "$OUTPUT"
fi

run_expecting_failure "$RUN_ID-nomount" "${auth_env[@]}" -e PUID=$puid -e PGID=$pgid
if [ "$EXIT_CODE" != 0 ]; then ok "nothing mounted at the library stops the start"; else fail "nothing mounted at the library stops the start" "$OUTPUT"; fi
check_contains "and says nothing is mounted" "Nothing is mounted" "$OUTPUT"
check_contains "and names create_host_path" "create_host_path: false" "$OUTPUT"

# the long form of a bind mount does not make a missing source, so the container does not start
run_expecting_failure "$RUN_ID-nosource" "${auth_env[@]}" --mount "type=bind,source=$WORK/does-not-exist,target=/data/library"
if [ "$EXIT_CODE" != 0 ]; then ok "a bind mount whose source is missing stops the container"; else fail "a bind mount whose source is missing stops the container" "$OUTPUT"; fi
if [ ! -e "$WORK/does-not-exist" ]; then ok "and no folder is made on the host"; else fail "and no folder is made on the host"; fi

run_expecting_failure "$RUN_ID-nologin" -e PUID=$puid -e PGID=$pgid -v "$WORK/library:/data/library" -v "$WORK/export:/data/export" -v "$WORK/config:/data/config"
if [ "$EXIT_CODE" != 0 ]; then ok "no login configured stops the start"; else fail "no login configured stops the start" "$OUTPUT"; fi
check_contains "and says there is no way without one" "never runs without" "$OUTPUT"

run_expecting_failure "$RUN_ID-overlap" "${auth_env[@]}" -e PUID=$puid -e PGID=$pgid -e HITOMI_EXPORT_DIR=/data/library/export \
    -v "$WORK/library:/data/library" -v "$WORK/config:/data/config"
if [ "$EXIT_CODE" != 0 ]; then ok "an export directory inside the library stops the start"; else fail "an export directory inside the library stops the start" "$OUTPUT"; fi
check_contains "and says why" "inside the library" "$OUTPUT"
check_equals "and leaves nothing in the library" 0 "$(find "$WORK/library" -mindepth 1 | wc -l)"

run_expecting_failure "$RUN_ID-badpuid" "${auth_env[@]}" -e PUID=abc
if [ "$EXIT_CODE" != 0 ]; then ok "a PUID that is no number stops the start"; else fail "a PUID that is no number stops the start" "$OUTPUT"; fi
check_contains "and says so" "PUID has to be a number" "$OUTPUT"

run_expecting_failure "$RUN_ID-badumask" "${auth_env[@]}" -e UMASK=999
check_contains "a UMASK that is no octal number is refused" "UMASK has to be an octal number" "$OUTPUT"

echo
echo "$((checks - failures)) of $checks checks passed"
[ "$failures" = 0 ]
