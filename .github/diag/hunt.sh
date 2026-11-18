#!/bin/bash
# Builds hosted from a cold home and runs its image many times, sampling any build or run that
# stalls. Usage: hunt.sh <nan> <target> <out> <builds> <runs>
set -u
NAN=$1; TARGET=$2; OUT=$3; BUILDS=$4; RUNS=$5
mkdir -p "$OUT"
work=$(mktemp -d)
cp -R bootstrap/nan/tests/programs/hosted "$work/hosted"
rm -rf "$work/hosted/expected"
perl -pi -e "s/^    targets .*/    targets $TARGET/" "$work/hosted/package.nan"
dump() {
  sudo -n sample "$1" 3 -file "$OUT/$2.sample.txt" > /dev/null 2>&1
  sudo -n lldb --batch -p "$1" -o "command script import $GITHUB_WORKSPACE/.github/diag/tasks.py" -o "nan_tasks" -o "thread backtrace all" > "$OUT/$2.lldb.txt" 2>&1
}
watch() {
  local pid=$1 limit=$2 label=$3 start=$SECONDS
  while kill -0 "$pid" 2> /dev/null; do
    if (( SECONDS - start >= limit )); then
      echo "STALL $label after ${limit}s"
      dump "$pid" "$label"
      kill -9 "$pid"
      return 1
    fi
    sleep 0.2
  done
  return 0
}
for i in $(seq 1 "$BUILDS"); do
  rm -rf "$work/.nan" "$work/home"
  mkdir -p "$work/home"
  start=$SECONDS
  (cd "$work" && HOME="$work/home" "$NAN" build hosted > "$OUT/build$i.log" 2>&1) &
  pid=$!
  sleep 1
  builder=$(pgrep -P "$pid" | head -1)
  watch "${builder:-$pid}" 240 "build$i"
  wait "$pid"
  echo "build $i: status $? in $((SECONDS - start))s"
done
image="$work/.nan/images/$TARGET/hosted"
expected=$(tail -n +2 "bootstrap/nan/tests/programs/hosted/expected/$TARGET")
times=()
for i in $(seq 1 "$RUNS"); do
  start=$(perl -MTime::HiRes=time -e 'printf "%.3f", time')
  (cd "$work" && "$image" < /dev/null > "$work/run.stdout" 2> "$work/run.stderr") &
  pid=$!
  sleep 0.05
  child=$(pgrep -P "$pid" | head -1)
  if ! watch "${child:-$pid}" 30 "run$i"; then
    cat "$work/run.stdout" "$work/run.stderr" > "$OUT/run$i.out"
    wait "$pid"
    continue
  fi
  wait "$pid"
  status=$?
  cat "$work/run.stdout" "$work/run.stderr" > "$work/run.out"
  end=$(perl -MTime::HiRes=time -e 'printf "%.3f", time')
  times+=("$(perl -e "printf '%.2f', $end - $start")")
  if [ "$status" != 3 ] || [ "$(cat "$work/run.out")" != "$expected" ]; then
    echo "run $i: status $status, output differs"
    cp "$work/run.out" "$OUT/run$i.out"
  fi
  rm -rf "$work/notes"
done
printf '%s\n' "${times[@]}" | sort -n | awk '{a[NR]=$1} END {print "runs " NR ", min " a[1] "s, median " a[int(NR/2)+1] "s, max " a[NR] "s"}'
