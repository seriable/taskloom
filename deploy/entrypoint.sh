#!/bin/sh
set -eu
# Secret values are inherited only by the process. Never echo them or include
# them in command-line arguments. File mounts must be supplied by the operator.
if [ -n "${TASKLOOM_DATABASE_URL_FILE:-}" ]; then
  TASKLOOM_DATABASE_URL=$(cat "$TASKLOOM_DATABASE_URL_FILE")
  export TASKLOOM_DATABASE_URL
fi
if [ -n "${TASKLOOM_MODEL_KEY_FILE:-}" ]; then
  TASKLOOM_MODEL_KEY=$(cat "$TASKLOOM_MODEL_KEY_FILE")
  export TASKLOOM_MODEL_KEY
fi
exec "$@"
