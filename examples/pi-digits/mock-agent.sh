#!/bin/sh
# Mock agent: reads value.txt, steps 30% of the way toward π.
# Deliberately regresses by -0.5 on every 4th iteration (iter % 4 == 0, iter>0),
# producing >=1 discard inside a 6-iter run.
set -eu
iter="${1:-0}"
v=$(cat value.txt)
case $(( iter % 4 )) in
  0) new=$(awk -v v="$v" 'BEGIN { print v - 0.5 }') ;;
  *) new=$(awk -v v="$v" 'BEGIN { pi=3.141592653589793; print v + (pi-v) * 0.3 }') ;;
esac
# iter 0 should not trigger regression (only iter 4, 8, ... do).
if [ "$iter" -eq 0 ]; then
  new=$(awk -v v="$v" 'BEGIN { pi=3.141592653589793; print v + (pi-v) * 0.3 }')
fi
printf "%s\n" "$new" > value.txt
