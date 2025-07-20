#!/bin/bash

LOG_DIR="./logs"
mkdir -p "$LOG_DIR"

while true; do
    TIMESTAMP=$(date '+%Y-%m-%d_%H-%M-%S')
    LOGFILE="$LOG_DIR/rocket_$TIMESTAMP.log"
    echo "[$TIMESTAMP] Starting Rocket server..." | tee -a "$LOG_DIR/monitor.log"

    ./target/release/tui_chat_server >> "$LOGFILE" 2>&1
    EXIT_CODE=$?

    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Rocket exited with code $EXIT_CODE" | tee -a "$LOG_DIR/monitor.log"

    # Optional investigation/logging (basic example)
    echo ">>> Last 10 lines of log:" >> "$LOG_DIR/monitor.log"
    tail -n 10 "$LOGFILE" >> "$LOG_DIR/monitor.log"

    # Wait 2 seconds before restarting
    sleep 2
done

