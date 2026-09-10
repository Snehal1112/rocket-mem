#!/usr/bin/env bash
# Shared plain-shell assertion helpers for scripts/tests/replication-health-*.sh. No test
# framework is installed in this repo (no bats) -- these few functions are the harness: print
# PASS/FAIL per assertion, track a running failure count, and let the caller decide the
# process exit code from that count at the end. Source this, don't execute it.
TESTS_RUN=0
TESTS_FAILED=0

assert_eq() { # $1=actual $2=expected $3=description
  TESTS_RUN=$((TESTS_RUN + 1))
  if [ "$1" = "$2" ]; then
    echo "  PASS: $3"
  else
    echo "  FAIL: $3 -- expected [$2], got [$1]"
    TESTS_FAILED=$((TESTS_FAILED + 1))
  fi
}

assert_contains() { # $1=haystack $2=needle $3=description
  TESTS_RUN=$((TESTS_RUN + 1))
  if [[ "$1" == *"$2"* ]]; then
    echo "  PASS: $3"
  else
    echo "  FAIL: $3 -- expected to find [$2] in:"
    echo "        $1"
    TESTS_FAILED=$((TESTS_FAILED + 1))
  fi
}

assert_not_contains() { # $1=haystack $2=needle $3=description
  TESTS_RUN=$((TESTS_RUN + 1))
  if [[ "$1" != *"$2"* ]]; then
    echo "  PASS: $3"
  else
    echo "  FAIL: $3 -- did not expect to find [$2] in:"
    echo "        $1"
    TESTS_FAILED=$((TESTS_FAILED + 1))
  fi
}

report_and_exit() { # $1=suite name
  echo "--- $1: $TESTS_RUN assertion(s), $TESTS_FAILED failure(s) ---"
  [ "$TESTS_FAILED" -eq 0 ]
}
