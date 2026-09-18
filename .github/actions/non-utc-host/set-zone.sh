#!/bin/sh
# Move this machine's HOST time zone to America/St_Johns (Newfoundland) and
# fail unless it took. Linux and macOS; Windows uses tzutil in action.yml.
#
# Runs in two places, which is why it is a script rather than inline YAML:
#   - on a GitHub runner, via .github/actions/non-utc-host (needs sudo);
#   - INSIDE a cibuildwheel manylinux / musllinux container, via
#     CIBW_BEFORE_TEST_LINUX (already root, no sudo). A container does not
#     inherit the runner's /etc/localtime, so the host step alone would leave
#     the wheel smoke tests on UTC while the host's self-check passed.
#
# See action.yml for why Newfoundland and why the host zone rather than TZ.
set -eu

ZONE=America/St_Johns

if [ "$(id -u)" = 0 ]; then SUDO=; else SUDO=sudo; fi

zonefile() {
    for d in /var/db/timezone/zoneinfo /usr/share/zoneinfo; do
        if [ -e "$d/$ZONE" ]; then
            echo "$d/$ZONE"
            return 0
        fi
    done
    return 1
}

if ! z=$(zonefile); then
    # Minimal container images (musllinux is Alpine) can ship without tzdata.
    $SUDO apk add --no-cache tzdata 2>/dev/null ||
        $SUDO dnf install -y tzdata 2>/dev/null ||
        $SUDO yum install -y tzdata 2>/dev/null ||
        $SUDO apt-get install -y tzdata 2>/dev/null || true
    z=$(zonefile) || {
        echo "no zoneinfo for $ZONE on this image" >&2
        exit 1
    }
fi

$SUDO ln -sf "$z" /etc/localtime
if [ "$(uname)" = Linux ]; then
    echo "$ZONE" | $SUDO tee /etc/timezone >/dev/null
fi

# The self-check: if the change silently did not take, every test after this
# would pass vacuously on a UTC host again. `time.timezone` is the STANDARD
# (non-DST) offset in seconds west of UTC; Newfoundland's is 3:30.
unset TZ
py=$(command -v python3 || command -v python)
"$py" -c "import time; assert time.timezone == 12600, ('host zone did not move', time.timezone, time.tzname); print('host zone OK:', time.tzname, -time.timezone / 3600, 'h')"
date
