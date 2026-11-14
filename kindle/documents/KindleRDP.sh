#!/bin/sh
# Name: KindleRDP
# Author: KindleRDP
# Icon: /mnt/us/kindlerdp/icon.png

cd /mnt/us/kindlerdp || exit 1

restore() {
    lipc-set-prop com.lab126.powerd preventScreenSaver 0
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
killall -STOP awesome 2>/dev/null

./kindlerdp >log.txt 2>&1
