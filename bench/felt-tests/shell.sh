#!/bin/bash
# Make starts one shell per recipe line. Keep that rule, including exit in if.
printf '%s\0' "$2" >> "$FELT_CASE/commands"
# Remove links at output paths before a command can open them for writing.
# This also protects source files if a later felt revision reuses a fixture name.
python3 "$(dirname "$0")/unlink-outputs.py" "$2" || exit 125
/bin/bash "$@"
rc=$?
if [ "$rc" -ne 0 ] && [ ! -e "$FELT_CASE/failure" ]; then
    printf '%s\n' "$rc" > "$FELT_CASE/failure"
fi
exit "$rc"
