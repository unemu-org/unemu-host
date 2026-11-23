#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Keep tracked and untracked text safe to publish: personal checkout paths,
# private hosts and private addresses do not belong in committed files, and
# the CPU core's other consumers stay anonymous. These are the mechanical
# rules adapted from unemu-core-api/scripts/publication-guard.sh.
#
# Usage: scripts/publication-guard.sh [--self-test]
#
# Run it from any directory. It scans tracked files and untracked,
# non-ignored files; grep -I skips binary contents. Every check reads the text
# with LC_ALL=C and needs only bash 3.2, git, grep, sed and awk, so it runs the
# same on macOS and Linux. The text checks skip the guard itself, because it
# defines the forbidden spellings, and Cargo.lock, which is generated; Cargo.lock
# has its own check: every dependency comes from crates.io or a github.com git
# repository.
#
# Checks:
# - private filesystem paths: home directories, macOS volumes and temporary
#   directories, Windows profiles, /root, and /mnt, /media and /run/media mounts;
# - names of the CPU core's other consumers;
# - e-mail addresses outside example domains and the allowlist below; the user
#   of a URL or of an scp-style remote (an SSH login) is not an e-mail address;
# - RFC 1918 and carrier-grade NAT (100.64.0.0/10) IPv4 addresses; network
#   ranges in CIDR notation are not addresses;
# - private hostnames, read only from the comma-separated environment variable
#   PUBLICATION_GUARD_PRIVATE_HOSTS so they never appear in the repository;
#   without it, this check is skipped, so it runs only where someone sets it,
#   never in CI. A hit prints only the file and line, never the name.
#
# --self-test runs the guard over small fixture directories, one clean and
# one per rule, and fails unless the clean one passes and every other fails.
set -euo pipefail
export LC_ALL=C

guard_path="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"

check_files() {
  # Absolute private path forms. The ones without a user directory must start a
  # path, so a URL or a relative path that merely contains them does not match.
  path_start='(^|[^[:alnum:]_.~-])'
  pattern='ps4|ps5|playstation|orbis|unemups4|/home/[^/[:space:]]+|/Users/[^/[:space:]]+|~/src/'
  pattern+="|${path_start}(/volumes/|/private/(var|tmp)/|/var/folders/|/root/|/mnt/[^/[:space:]]+"
  pattern+='|/media/[^/[:space:]]+/|/run/media/[^/[:space:]]+/)'
  pattern+='|[[:alpha:]]:[/\\]+users[/\\]'
  # An address preceded by '/' is a URL's user information, not an e-mail address.
  # The optional tail catches the ':path' of an scp-style remote, which the
  # report then skips.
  email_re='(^|[^/[:alnum:]._%+-])[[:alnum:]._%+-]+@[[:alnum:]-]+(\.[[:alnum:]-]+)*\.[[:alpha:]]{2,}(:[[:alnum:]_.~/-])?'
  email_remote=':[^:]$'
  # The commit trailer of the coding agent and example domains.
  email_allowed='^(noreply@anthropic\.com|.*@(([[:alnum:]-]+\.)*example\.(org|com|net)|([[:alnum:]-]+\.)+example))$'
  # RFC 1918 and carrier-grade NAT; an optional prefix length marks a CIDR range,
  # which the report skips.
  private_ip_re='(^|[^[:alnum:].])(10(\.[0-9]{1,3}){3}|172\.(1[6-9]|2[0-9]|3[01])(\.[0-9]{1,3}){2}|192\.168(\.[0-9]{1,3}){2}'
  private_ip_re+='|100\.(6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7])(\.[0-9]{1,3}){2})(/[0-9]{1,2})?($|[^0-9.]|\.$|\.[^0-9])'
  private_ip_range='[0-9]/[0-9]'
  private_ip_allowed='^$'
  lock_source_allowed='^source = "(registry\+https://github\.com/rust-lang/crates\.io-index|sparse\+https://index\.crates\.io/|git\+https://github\.com/[^"[:space:]]+)"$'
  rc=0

  local all_files=("$@")
  local files=()
  for file in "${all_files[@]}"; do
    case "$file" in
      scripts/publication-guard.sh|Cargo.lock) ;;
      *) files+=("$file") ;;
    esac
  done

  # Print the hits of one check whose matches are filtered by an allowlist:
  # grep's "file:line:match" lines on stdin; matches containing the ERE in $3
  # are not values at all; the rest are reduced to the bare value by the pattern
  # in $1, then values matching the ERE in $2 are dropped.
  # The patterns travel in the environment: awk -v would rewrite their backslashes.
  report_unlisted() {
    GUARD_VALUE="$1" GUARD_ALLOWED="$2" GUARD_SKIP="$3" awk '{
      text = $0
      sub(/^[^:]*:[0-9]*:/, "", text)
      if (text ~ ENVIRON["GUARD_SKIP"]) next
      if (!match(text, ENVIRON["GUARD_VALUE"])) next
      found = tolower(substr(text, RSTART, RLENGTH))
      if (found ~ ENVIRON["GUARD_ALLOWED"]) next
      print
    }'
  }

  # Run grep over every scanned file at once; status 1 means no match.
  scan() {
    local status=0
    LC_ALL=C grep "$@" || status=$?
    if (( status > 1 )); then
      exit "$status"
    fi
  }

  if (( ${#files[@]} )); then
    hits=$(scan -HIniE -- "$pattern" "${files[@]}")
    if [[ -n $hits ]]; then
      printf '%s\n' "$hits"
      printf '%s\n' 'publication guard: private path or other consumer named' >&2
      rc=1
    fi

    hits=$(scan -HIinoE -- "$email_re" "${files[@]}" |
      report_unlisted '[[:alnum:]._%+-]+@[[:alnum:].-]*[[:alnum:]]' "$email_allowed" "$email_remote")
    if [[ -n $hits ]]; then
      printf '%s\n' "$hits"
      printf '%s\n' 'publication guard: e-mail address outside the allowlist' >&2
      rc=1
    fi

    hits=$(scan -HIinoE -- "$private_ip_re" "${files[@]}" |
      report_unlisted '[0-9]+[.][0-9]+[.][0-9]+[.][0-9]+' "$private_ip_allowed" "$private_ip_range")
    if [[ -n $hits ]]; then
      printf '%s\n' "$hits"
      printf '%s\n' 'publication guard: private IPv4 address' >&2
      rc=1
    fi
  fi

  host_patterns=()
  IFS=',' read -r -a hosts <<< "${PUBLICATION_GUARD_PRIVATE_HOSTS:-}"
  for host in ${hosts[@]+"${hosts[@]}"}; do
    host=$(printf '%s' "$host" | LC_ALL=C sed -E 's/^[[:space:]]+//; s/[[:space:]]+$//')
    if [[ -n $host ]]; then
      host_patterns+=(-e "$host")
    fi
  done
  if (( ${#host_patterns[@]} )); then
    # Only file and line: the names stay out of logs and terminals.
    hits=$(scan -HIinF "${host_patterns[@]}" -- "${all_files[@]}" | cut -d: -f1,2)
    if [[ -n $hits ]]; then
      printf '%s\n' "$hits"
      printf '%s\n' 'publication guard: private hostname from PUBLICATION_GUARD_PRIVATE_HOSTS' >&2
      rc=1
    fi
  fi

  if [[ -f Cargo.lock ]]; then
    hits=$(scan -n '^source = ' Cargo.lock |
      LC_ALL=C GUARD_ALLOWED="$lock_source_allowed" awk '{
        text = $0
        sub(/^[0-9]*:/, "", text)
        if (text !~ ENVIRON["GUARD_ALLOWED"]) print "Cargo.lock:" $0
      }')
    if [[ -n $hits ]]; then
      printf '%s\n' "$hits"
      printf '%s\n' 'publication guard: Cargo.lock source outside crates.io and github.com' >&2
      rc=1
    fi
  fi

  if (( rc )); then
    printf '%s\n' 'publication guard: fix the publication diagnostics above.' >&2
  fi
  return "$rc"
}

if [[ ${1:-} == --self-test ]]; then
  work=$(mktemp -d)
  trap 'rm -rf "$work"' EXIT
  failures=0

  # Build a fixture directory holding the guard and one file, then run the
  # guard there. The fixture texts are assembled from pieces so this file
  # does not hold them whole.
  run_fixture() {
    local name="$1" text="$2" hosts="${3:-}" repo="$work/$1"
    mkdir -p "$repo/scripts"
    cp "$guard_path" "$repo/scripts/publication-guard.sh"
    printf '%s\n' "$text" > "$repo/notes.txt"
    (cd "$repo" && PUBLICATION_GUARD_PRIVATE_HOSTS="$hosts" \
      check_files scripts/publication-guard.sh notes.txt) > "$work/$name.log" 2>&1
  }

  expect_pass() {
    if run_fixture "$@"; then
      printf 'ok    %s passes\n' "$1"
    else
      printf 'FAIL  %s should pass:\n' "$1"; sed 's/^/      /' "$work/$1.log"
      failures=$((failures + 1))
    fi
  }

  # The second argument is the diagnostic the failure must carry, so a
  # fixture that fails for another reason does not count.
  expect_fail() {
    local name="$1" diagnostic="$2"
    shift 2
    if run_fixture "$name" "$@"; then
      printf 'FAIL  %s should fail\n' "$name"
      failures=$((failures + 1))
    elif ! grep -qF -- "publication guard: $diagnostic" "$work/$name.log"; then
      printf 'FAIL  %s should report "%s":\n' "$name" "$diagnostic"
      sed 's/^/      /' "$work/$name.log"
      failures=$((failures + 1))
    else
      printf 'ok    %s fails\n' "$name"
    fi
  }

  at='@'
  path_hit='private path or other consumer named'
  expect_pass clean "See https://github.com/unemu-org/unemu-core-api and git${at}github.com:unemu-org/x.git; mail someone${at}example.org; the range 10.0.0.0/8."
  expect_fail home-directory "$path_hit" "Built in /Users""/someone/src/checkout."
  expect_fail linux-home "$path_hit" "Built in /home""/someone/checkout."
  expect_fail temp-directory "$path_hit" "Logs in /private""/tmp/run."
  expect_fail windows-profile "$path_hit" "C:\\Users""\\someone\\checkout"
  expect_fail other-consumer "$path_hit" "Also used by a PlayStation"" emulator."
  expect_fail e-mail "e-mail address" "Write to someone${at}mail.invalid-host.net."
  expect_fail private-ipv4 "private IPv4" "The board answers at 192.168"".1.20."
  expect_fail cgnat-ipv4 "private IPv4" "The board answers at 100.64"".1.20."
  expect_fail private-host "private hostname" "Mounted from buildbox-7 overnight." "buildbox-7"

  if (( failures )); then
    printf 'publication guard self-test: %d failure(s)\n' "$failures" >&2
    exit 1
  fi
  printf 'publication guard self-test: all passed\n'
  exit 0
fi

case "${1:-}" in
  '') ;;
  *) printf 'usage: scripts/publication-guard.sh [--self-test]\n' >&2; exit 2 ;;
esac
cd "$(dirname "$guard_path")/.."
all_files=()
while IFS= read -r -d '' file; do
  all_files+=("$file")
done < <(git ls-files --cached --others --exclude-standard -z)
check_files "${all_files[@]}"
