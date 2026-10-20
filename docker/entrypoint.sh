#!/bin/sh
# Start hitomi-server as the user PUID:PGID, with the umask UMASK
#
# The files the server writes belong to PUID:PGID and are readable by whom UMASK allows, so a
# library on a NAS share gets an owner and rights that other programs can work with.
#
# Only the config directory is handed to that user. The library and the export directory are
# never touched here: a recursive chown of a big library takes long, and it fails on an NFS
# share with root squash. If the user can not write there, the server says so at the start.
set -eu

fail() {
    echo "hitomi-downloader: $*" >&2
    exit 1
}

# `docker run <image> hash-password` and the healthcheck need nothing prepared
prepare=1
case "${1:-}" in
    hash-password | healthcheck)
        set -- hitomi-server "$@"
        prepare=
        ;;
esac

PUID="${PUID:-1000}"
PGID="${PGID:-1000}"
UMASK="${UMASK:-022}"

case "$PUID" in '' | *[!0-9]*) fail "PUID has to be a number, not '$PUID'" ;; esac
case "$PGID" in '' | *[!0-9]*) fail "PGID has to be a number, not '$PGID'" ;; esac
case "$UMASK" in '' | *[!0-7]*) fail "UMASK has to be an octal number such as 022, not '$UMASK'" ;; esac
[ "${#UMASK}" -le 4 ] || fail "UMASK has to be an octal number such as 022, not '$UMASK'"

if [ -n "${TZ:-}" ] && [ ! -e "/usr/share/zoneinfo/$TZ" ]; then
    echo "hitomi-downloader: warning: the time zone '$TZ' is unknown, times are in UTC" >&2
fi

umask "$UMASK"

# Started with `user:` in the compose file or `--user`: this script can not change the user, and
# the files belong to whoever it runs as
if [ "$(id -u)" != 0 ]; then
    exec "$@"
fi

CONFIG_DIR="${HITOMI_CONFIG_DIR:-/data/config}"
[ -z "$prepare" ] || mkdir -p "$CONFIG_DIR"

if [ "$PUID" = 0 ]; then
    echo "hitomi-downloader: warning: running as root, set PUID and PGID to run as another user" >&2
    exec "$@"
fi

if [ -n "$prepare" ]; then
    # It is small and local, and what an earlier run with another PUID left there has to stay usable
    chown -R "$PUID:$PGID" "$CONFIG_DIR" ||
        echo "hitomi-downloader: warning: could not hand '$CONFIG_DIR' to $PUID:$PGID" >&2
fi

export HOME="$CONFIG_DIR"
exec setpriv --reuid="$PUID" --regid="$PGID" --clear-groups --no-new-privs "$@"
