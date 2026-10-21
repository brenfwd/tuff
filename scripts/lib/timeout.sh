# Sourced by scripts that bound a command's run time: run_with_timeout SECS CMD...
# Uses GNU timeout (Linux), gtimeout (Homebrew coreutils on macOS) or, failing
# both, perl's alarm, which stock macOS ships.
run_with_timeout() {
  local secs=$1
  shift
  if command -v timeout >/dev/null 2>&1; then
    timeout "$secs" "$@"
  elif command -v gtimeout >/dev/null 2>&1; then
    gtimeout "$secs" "$@"
  else
    perl -e 'alarm shift; exec @ARGV or die "exec $ARGV[0]: $!\n"' "$secs" "$@"
  fi
}
