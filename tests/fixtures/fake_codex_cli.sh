#!/bin/sh
set -eu

printf 'SESSION_READY\r\n'
trap 'printf "RESIZED:%s\r\n" "$(stty size 2>/dev/null || printf unknown)"' WINCH
while IFS= read -r line; do
  case "$line" in
    exit)
      printf 'FAKE_EXIT\r\n'
      exit 0
      ;;
    flood)
      /bin/dd if=/dev/zero bs=1048576 count=10 2>/dev/null | /usr/bin/tr '\0' x
      ;;
    *)
      printf 'ECHO:%s\r\n' "$line"
      ;;
  esac
done
