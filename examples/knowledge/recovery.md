# Recovery

An interrupted non-idempotent tool requires reconciliation before recovery.
Record its observed external result before resuming; do not silently repeat it.
