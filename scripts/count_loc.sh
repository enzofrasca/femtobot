#!/usr/bin/env bash
set -euo pipefail

ROOT="${1:-.}"

if [[ ! -d "$ROOT" ]]; then
  echo "Error: '$ROOT' is not a directory." >&2
  exit 1
fi

is_code_file() {
  case "$1" in
    *.rs|*.py|*.js|*.jsx|*.ts|*.tsx|*.java|*.kt|*.go|*.c|*.h|*.cpp|*.hpp|*.cs|*.swift|*.rb|*.php|*.sh|*.bash|*.zsh|*.lua|*.sql|*.html|*.css|*.scss)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

counts_file="$(mktemp)"
trap 'rm -f "$counts_file"' EXIT

if git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  while IFS= read -r -d '' rel_path; do
    is_code_file "$rel_path" || continue
    full_path="$ROOT/$rel_path"
    [[ -f "$full_path" ]] || continue

    line_count="$(wc -l < "$full_path" | tr -d ' ')"
    ext="${rel_path##*.}"
    printf "%s\t%s\n" "$ext" "$line_count" >> "$counts_file"
  done < <(git -C "$ROOT" ls-files -z)
else
  while IFS= read -r -d '' full_path; do
    is_code_file "$full_path" || continue
    [[ -f "$full_path" ]] || continue

    line_count="$(wc -l < "$full_path" | tr -d ' ')"
    ext="${full_path##*.}"
    printf "%s\t%s\n" "$ext" "$line_count" >> "$counts_file"
  done < <(find "$ROOT" -type f -print0)
fi

if [[ ! -s "$counts_file" ]]; then
  echo "No code files found."
  exit 0
fi

awk -F'\t' -v root="$ROOT" '
  {
    files += 1
    lines_by_ext[$1] += $2
    total_lines += $2
  }
  END {
    printf "Counted %d code files in '\''%s'\''\n", files, root
    printf "%-12s %12s\n", "Extension", "Lines"
    printf "%-12s %12s\n", "---------", "-----"

    for (ext in lines_by_ext) {
      printf "%s\t%d\n", ext, lines_by_ext[ext] | "sort -k2,2nr"
    }
    close("sort -k2,2nr")

    printf "%-12s %12d\n", "TOTAL", total_lines
  }
' "$counts_file"
