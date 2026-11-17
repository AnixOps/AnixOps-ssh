#!/usr/bin/env bash
# Waits until both test servers answer with an SSH banner.
set -uo pipefail
deadline=$((SECONDS + ${1:-90}))
for port in 2222 2223; do
  while :; do
    if exec 3<>"/dev/tcp/127.0.0.1/$port" 2>/dev/null; then
      if read -r -t 2 banner <&3 && [[ "$banner" == SSH-* ]]; then
        echo "port $port: $banner"
        exec 3<&- 3>&-
        break
      fi
      exec 3<&- 3>&-
    fi
    if ((SECONDS >= deadline)); then
      echo "port $port did not answer with an SSH banner in time" >&2
      exit 1
    fi
    sleep 1
  done
done
