#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Checks that code and documentation describe Trama as it is: no plans,
# project phases, reminders or references to document sections. The rules
# are in CONTRIBUTING.md, under "Documentation".
#
#   scripts/check-docs.sh             every tracked file
#   scripts/check-docs.sh FILE...     only these files
#   scripts/check-docs.sh --changed   files changed since the last commit
#   scripts/check-docs.sh --hook      a Claude Code PostToolUse event on stdin:
#                                     the file the tool just wrote
#   scripts/check-docs.sh --stop      a Claude Code Stop event on stdin:
#                                     files changed since the last commit
#
# Exits with 1 when something is found (2 in the hook modes, which makes
# Claude Code show the report to the agent).

set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
self="scripts/check-docs.sh"

# Words and phrases that talk about plans. Matched ignoring case, as whole
# words: the characters around them must not be letters, digits or "_".
plan_words=(
    'milestones?'
    'hitos?'
    'roadmap'
    'planned'
    'phase [0-9]+'
    'fase [0-9]+'
    'for now'
    'not yet'
    'in the future'
    'coming soon'
    'eventually'
    'sections? [0-9]+'
)

# Reminder markers. Matched with case, so ordinary words are left alone.
markers=('TODO' 'FIXME' 'XXX' 'HACK')

edge='(^|[^[:alnum:]_])'
edge_end='([^[:alnum:]_]|$)'

joined() {
    local IFS='|'
    echo "$*"
}

plan_pattern="$edge($(joined "${plan_words[@]}"))$edge_end"
marker_pattern="$edge($(joined "${markers[@]}"))$edge_end"

checked_file() {
    case "$1" in
        "$self" | LICENSE | Cargo.lock) return 1 ;;
        *.rs | *.md | *.toml | *.yml | *.yaml | *.sh) return 0 ;;
        *) return 1 ;;
    esac
}

tracked_files() {
    git -C "$root" ls-files
}

changed_files() {
    git -C "$root" diff --name-only HEAD 2>/dev/null || true
    git -C "$root" ls-files --others --exclude-standard
}

# Reads a Claude Code hook event from stdin and prints one of its fields.
event_field() {
    local input
    input=$(cat)
    if command -v jq >/dev/null 2>&1; then
        jq -r "$1 // empty" <<<"$input"
    elif command -v python3 >/dev/null 2>&1; then
        python3 -c '
import json, sys
value = json.load(sys.stdin)
for key in sys.argv[1].lstrip(".").split("."):
    value = value.get(key, {}) if isinstance(value, dict) else {}
print(value if value not in ({}, None) else "")
' "$1" <<<"$input"
    fi
}

exit_code=1
files=()
case "${1:-}" in
    --hook)
        exit_code=2
        path=$(event_field '.tool_input.file_path')
        case "$path" in
            "$root"/*) files=("${path#"$root"/}") ;;
            *) exit 0 ;;
        esac
        ;;
    --stop)
        exit_code=2
        # Claude Code sets this when the agent is already fixing what a
        # previous stop reported; checking again could loop forever.
        if [ "$(event_field '.stop_hook_active')" = "true" ]; then
            exit 0
        fi
        while IFS= read -r file; do files+=("$file"); done < <(changed_files)
        ;;
    --changed)
        while IFS= read -r file; do files+=("$file"); done < <(changed_files)
        ;;
    "")
        while IFS= read -r file; do files+=("$file"); done < <(tracked_files)
        ;;
    *)
        files=("$@")
        ;;
esac

found=0
report=""
for file in "${files[@]}"; do
    [ -f "$root/$file" ] || continue
    checked_file "$file" || continue
    hits=$(
        grep -nEi "$plan_pattern" "$root/$file" || true
        grep -nE "$marker_pattern" "$root/$file" || true
    )
    if [ -n "$hits" ]; then
        found=1
        while IFS= read -r hit; do
            report+="$file:$hit"$'\n'
        done <<<"$hits"
    fi
done

if [ "$found" -eq 1 ]; then
    {
        echo "The repository describes Trama as it is today. These lines talk about"
        echo "plans, leave reminders or point at document sections:"
        echo
        printf '%s' "$report" | sort -u
        echo
        echo "Rewrite them to describe what exists, or remove them. Plans and missing"
        echo "work go in GitHub issues (gh issue create). See CONTRIBUTING.md."
    } >&2
    exit "$exit_code"
fi
