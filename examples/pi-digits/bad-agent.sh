#!/bin/sh
set -eu
mkdir -p .autorize/pi
printf "tampered\n" > .autorize/pi/state.json
