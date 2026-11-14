#!/bin/sh
# Linux: builds kindlerdp for the Kindle and assembles dist/, which mirrors the Kindle's USB root.
# The Windows counterpart is package.ps1.
set -e
cd "$(dirname "$0")"

target=armv7-unknown-linux-musleabihf
# .cargo/config.toml names the Windows .cmd wrappers; these take their place.
export CC_armv7_unknown_linux_musleabihf="$PWD/tools/zigcc"
export AR_armv7_unknown_linux_musleabihf="$PWD/tools/zigar"

cargo build --release --target "$target"

rm -rf dist
cp -r kindle dist
cp "target/$target/release/kindlerdp" dist/kindlerdp/
echo "Done. Copy the contents of $PWD/dist to the Kindle's root folder."
