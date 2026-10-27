#!/bin/sh

# Runs browser test files repeatedly while every CPU is kept busy, to expose a
# wait that only holds on an idle machine. A race that fails one run in twenty
# on CI rarely shows in one quiet local run; saturating the cores stretches the
# gaps between frames, timers and network callbacks the way a loaded runner
# does.
#
#   scripts/browser-stress.sh [-n RUNS] [-j BURNERS] FILE...
#
# Exit status is 0 only when every run of every file passed, and the summary
# names each file's failures, because a failure here is the finding.
set -u

usage()
{
    echo "usage: ${0##*/} [-n RUNS] [-j BURNERS] FILE..." >&2
    exit 2
}

runs=10
burners=$(nproc 2> /dev/null || echo 4)
while getopts n:j: opt; do
    case $opt in
        n) runs=$OPTARG ;;
        j) burners=$OPTARG ;;
        *) usage ;;
    esac
done
shift $((OPTIND - 1))
[ "$#" -gt 0 ] || usage

# A count that is not a positive integer would run nothing and still report a
# pass, which is the one wrong answer a stress run must not give. Leading zeros
# and a length past what the shell compares are refused with the rest.
for count in "$runs" "$burners"; do
    case $count in
        '' | *[!0-9]* | 0*) ;;
        *) [ "${#count}" -le 6 ] && continue ;;
    esac
    echo "${0##*/}: -n and -j take a positive integer, not '$count'" >&2
    exit 2
done

# A failing run's output is kept: the failure under load is the finding, and a
# count alone does not say which assertion broke.
logs=$(mktemp -d)
pids=""
# ShellCheck cannot follow cleanup through the EXIT trap.
# shellcheck disable=SC2086,SC2317,SC2329
cleanup()
{
    [ -z "$pids" ] || kill $pids 2> /dev/null
    # Only an empty directory goes: one holding a failed run's output stays.
    rmdir "$logs" 2> /dev/null
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

i=0
while [ "$i" -lt "$burners" ]; do
    (while :; do :; done) &
    pids="$pids $!"
    i=$((i + 1))
done

status=0
index=0
for file in "$@"; do
    index=$((index + 1))
    failed=0
    n=0
    while [ "$n" -lt "$runs" ]; do
        n=$((n + 1))
        log=$logs/$index-${file##*/}.$n.log
        if ! node --test "$file" > "$log" 2>&1; then
            failed=$((failed + 1))
            echo "  run $n failed, output in $log"
        fi
    done
    echo "$file: $failed of $runs runs failed under $burners busy loops"
    [ "$failed" -eq 0 ] || status=1
done
[ "$status" -eq 0 ] && rm -rf "$logs"
exit "$status"
