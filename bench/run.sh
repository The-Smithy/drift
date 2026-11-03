#!/usr/bin/env bash
# Runs `drift bench` on the pinned repos in bench/corpus.txt, so everyone measures the same thing.
#
#   bench/run.sh                  print each repo's results
#   bench/run.sh --save DIR       save JSON reports to DIR
#   bench/run.sh --compare DIR    compare with reports saved earlier
#   bench/run.sh --check PERCENT  fail unless each repo saves at least this much
#
# Set DRIFT to the binary to measure. The default is this checkout's release build.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cache="${BENCH_CACHE:-$root/.bench-corpus}"

drift="${DRIFT:-$root/target/release/drift}"
if [ ! -x "$drift" ] && [ -x "$drift.exe" ]; then
  drift="$drift.exe"
fi
if [ ! -x "$drift" ]; then
  echo "No binary at $drift. Run 'cargo build --release', or set DRIFT." >&2
  exit 1
fi

save=""
compare=""
check=""
while [ $# -gt 0 ]; do
  case "$1" in
    --save) save="$2"; shift 2 ;;
    --compare) compare="$2"; shift 2 ;;
    --check) check="$2"; shift 2 ;;
    *) echo "Unknown option: $1" >&2; exit 2 ;;
  esac
done

[ -z "$save" ] || mkdir -p "$save"

status=0
while read -r name url commit; do
  case "$name" in ''|\#*) continue ;; esac

  directory="$cache/$name"
  if [ "$(git -C "$directory" rev-parse HEAD 2>/dev/null || true)" != "$commit" ]; then
    rm -rf "$directory"
    mkdir -p "$directory"
    # Line endings stay as committed, so results are the same on every platform.
    git -C "$directory" init -q
    git -C "$directory" config core.autocrlf false
    git -C "$directory" remote add origin "$url"
    git -C "$directory" fetch -q --depth 1 origin "$commit"
    git -C "$directory" checkout -q FETCH_HEAD
  fi

  arguments=(bench "$directory")
  [ -z "$compare" ] || arguments+=(--compare "$compare/$name.json")
  [ -z "$check" ] || arguments+=(--check "$check")

  if [ -n "$save" ]; then
    "$drift" "${arguments[@]}" --json > "$save/$name.json" || status=1
  else
    "$drift" "${arguments[@]}" || status=1
    echo
  fi
done < "$root/bench/corpus.txt"

exit "$status"
