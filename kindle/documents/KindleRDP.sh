#!/bin/sh
# Name: KindleRDP
# Author: KindleRDP
# Icon: /mnt/us/kindlerdp/icon.png

cd /mnt/us/kindlerdp || exit 1

# On newer firmware the status bar (clock) is its own job and keeps drawing over the app.
# Same handling as KOReader's launcher.
STATUSBAR=no
[ -f /etc/upstart/statusbar.conf ] && STATUSBAR=yes

restore() {
    lipc-set-prop com.lab126.powerd preventScreenSaver 0
    [ "$STATUSBAR" = yes ] && start statusbar
    killall -CONT awesome 2>/dev/null
    lipc-set-prop com.lab126.pillow disableEnablePillow enable
    # The Kindle UI does not know the screen was overwritten: reopen Home to redraw it.
    lipc-set-prop com.lab126.appmgrd start app://com.lab126.booklet.home
}
trap restore EXIT
trap 'exit 1' INT TERM HUP

lipc-set-prop com.lab126.cmd wirelessEnable 1
lipc-set-prop com.lab126.powerd preventScreenSaver 1
# Same approach as KOReader: freeze the Kindle UI instead of stopping it, so exit is instant.
lipc-set-prop com.lab126.pillow disableEnablePillow disable
[ "$STATUSBAR" = yes ] && stop statusbar
# The Kindle UI repaints after the status bar goes away. Let it finish, or it paints over
# the app's first screen.
sleep 1
killall -STOP awesome 2>/dev/null

./kindlerdp >log.txt 2>&1
