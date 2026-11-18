#!/bin/bash
# Boots kernel_steals until a boot stalls, dumping its scheduler. Usage: kernel_hunt.sh <nan> <out> <boots>
set -u
NAN=$1; OUT=$2; BOOTS=$3
mkdir -p "$OUT"
work=$(mktemp -d)
cp -R bootstrap/nan/tests/programs/kernel_steals "$work/kernel_steals"
rm -f "$work/kernel_steals/expected"
(cd "$work" && "$NAN" build kernel_steals) || exit 1
sha256sum "$work/.nan/images/x86_64-bare/kernel_steals.elf"
for i in $(seq 1 "$BOOTS"); do
  start=$SECONDS
  (cd "$work" && "$NAN" run kernel_steals > "$OUT/boot.out" 2>&1) &
  pid=$!
  stalled=0
  while kill -0 "$pid" 2> /dev/null; do
    if (( SECONDS - start >= 25 )); then
      stalled=1
      qemu=$(pgrep -f 'qemu-system-x86_64.*kernel_steals' | head -1)
      echo "STALL boot $i, emulator ${qemu:-none}"
      if [ -n "$qemu" ]; then
        dir=$(readlink "/proc/$qemu/cwd")
        python3 .github/diag/kernel.py "$dir/monitor.sock" 0x11e040 0x13f2c0 > "$OUT/boot$i.dump.txt" 2>&1
        sleep 10
        python3 .github/diag/kernel.py "$dir/monitor.sock" 0x11e040 0x13f2c0 > "$OUT/boot$i.dump2.txt" 2>&1
        cp "$OUT/boot.out" "$OUT/boot$i.out"
        kill -9 "$qemu"
      fi
      kill -9 "$pid" 2> /dev/null
      break
    fi
    sleep 0.2
  done
  wait "$pid" 2> /dev/null
  echo "boot $i: $((SECONDS - start))s stalled $stalled $(grep -c 'every token counted' "$OUT/boot.out")"
done
