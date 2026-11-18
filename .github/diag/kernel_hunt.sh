#!/bin/bash
# Boots variants of kernel_steals in turn on one runner, timing each boot and dumping the scheduler
# of the unchanged program when a boot stalls. Usage: kernel_hunt.sh <nan> <out> <rounds>
set -u
NAN=$1; OUT=$2; ROUNDS=$3
mkdir -p "$OUT"
variants=(as-is poll slice-50us)
changes=('' 's/return hw::Idle.Halt/return hw::Idle.Poll/' 's/return 5_000$/return 50_000/')
for v in 0 1 2; do
  work="$OUT/work-${variants[$v]}"
  mkdir -p "$work"
  cp -R bootstrap/nan/tests/programs/kernel_steals "$work/kernel_steals"
  rm -f "$work/kernel_steals/expected"
  [ -n "${changes[$v]}" ] && perl -pi -e "${changes[$v]}" "$work/kernel_steals/kernel_steals.nan"
  echo "${variants[$v]}: $(grep -cE 'return 50_000|hw::Idle\.Poll' "$work/kernel_steals/kernel_steals.nan") changed lines"
  (cd "$work" && "$NAN" build kernel_steals > /dev/null) || exit 1
  sha256sum "$work/.nan/images/x86_64-bare/kernel_steals.elf"
done
for i in $(seq 1 "$ROUNDS"); do
  for v in 0 1 2; do
    name=${variants[$v]}
    work="$OUT/work-$name"
    start=$SECONDS
    (cd "$work" && "$NAN" run kernel_steals > "$OUT/boot.out" 2>&1) &
    pid=$!
    stalled=0
    while kill -0 "$pid" 2> /dev/null; do
      if (( SECONDS - start >= 40 )); then
        stalled=1
        qemu=$(pgrep -f 'qemu-system-x86_64' | head -1)
        if [ "$name" = as-is ] && [ -n "$qemu" ]; then
          python3 .github/diag/kernel.py "$(readlink "/proc/$qemu/cwd")/monitor.sock" 0x11e040 0x13f2c0 > "$OUT/$name-$i.dump.txt" 2>&1
        fi
        [ -n "$qemu" ] && kill -9 "$qemu"
        kill -9 "$pid" 2> /dev/null
        break
      fi
      sleep 0.2
    done
    wait "$pid" 2> /dev/null
    echo "boot $name $i: $((SECONDS - start))s stalled $stalled counted $(grep -c 'every token counted' "$OUT/boot.out")"
  done
done
