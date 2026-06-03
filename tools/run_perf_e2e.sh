#!/bin/bash
set -euo pipefail

# Usage: ./run_perf_e2e.sh <pub_subs> <pure_subs> [top_n] [duration_sec] [output_dir]
# Example: ./run_perf_e2e.sh 80 720 45 120 ./perf-results

PUB_SUBS=${1:?Usage: $0 <pub_subs> <pure_subs> [top_n] [duration_sec] [output_dir]}
PURE_SUBS=${2:?Usage: $0 <pub_subs> <pure_subs> [top_n] [duration_sec] [output_dir]}
TOP_N=${3:-45}
DURATION=${4:-120}
OUTPUT_DIR=${5:-./perf-results}

TOTAL_SUBS=$((PUB_SUBS + PURE_SUBS))
REMOTE="admin@snk-dev-1.m10x.org"
SSH_KEY="$HOME/.ssh/keys/snk-dev-server.pem"
SSH="ssh -i $SSH_KEY $REMOTE"
SCP="scp -i $SSH_KEY"
TIMESTAMP=$(date +%Y%m%d-%H%M%S)
REMOTE_DIR="/tmp/moq-rs-perf-$TIMESTAMP"
LOCAL_DIR="$OUTPUT_DIR/$TIMESTAMP"

mkdir -p "$LOCAL_DIR"

echo "=== MOQ-RS E2E Performance Test ==="
echo "  Publishers (pub-sub): $PUB_SUBS"
echo "  Pure subscribers:     $PURE_SUBS"
echo "  Total subscribers:    $TOTAL_SUBS"
echo "  Top-N filter:         $TOP_N"
echo "  Duration:             ${DURATION}s"
echo "  Remote:               $REMOTE"
echo "  Remote dir:           $REMOTE_DIR"
echo "  Local output:         $LOCAL_DIR"
echo ""

# Step 1: Collect system info
echo "[1/7] Collecting system configuration..."
$SSH "bash -s" > "$LOCAL_DIR/sysinfo.txt" <<'SYSINFO'
echo "=== System Configuration ==="
echo "Hostname: $(hostname)"
echo "Date: $(date -u)"
echo "Kernel: $(uname -r)"
echo "Arch: $(uname -m)"
echo "CPU:"
lscpu | grep -E "^(Model name|CPU\(s\)|Thread|Core|Socket|CPU max)"
echo ""
echo "Memory:"
free -h | head -2
echo ""
echo "OS:"
cat /etc/os-release 2>/dev/null | grep -E "^(NAME|VERSION)" || true
echo ""
echo "Rust:"
source ~/.cargo/env 2>/dev/null
rustc --version 2>/dev/null || echo "unknown"
echo ""
echo "Git commit:"
cd ~/moq-rs-top-n && git log --oneline -1
echo "Git branch:"
cd ~/moq-rs-top-n && git branch --show-current
SYSINFO

echo "  Done."

# Step 2: Build on remote
echo "[2/7] Building on remote..."
$SSH "cd ~/moq-rs-top-n && source ~/.cargo/env && cargo build --release --bin moq-relay-ietf --bin moq-topn-test 2>&1 | tail -3"
echo "  Done."

# Step 3: Run perf test
echo "[3/7] Running perf test (${DURATION}s + overhead)..."
$SSH "bash -s" <<PERF_SCRIPT
set -e
cd ~/moq-rs-top-n
source ~/.cargo/env
mkdir -p $REMOTE_DIR

pkill -f moq-relay-ietf 2>/dev/null || true
sleep 1

# Start relay
./target/release/moq-relay-ietf --bind "[::]:4443" --tls-cert cert.pem --tls-key key.pem > $REMOTE_DIR/relay.log 2>&1 &
RELAY_PID=\$!
sleep 2

# Capture memory baseline
ps -o rss= -p \$RELAY_PID > $REMOTE_DIR/mem_before.txt

# Start perf recording
perf record -F 999 -p \$RELAY_PID -g -o $REMOTE_DIR/perf.data -- sleep $((DURATION + 15)) &
PERF_PID=\$!

# Run e2e test
./target/release/moq-topn-test -m e2e \\
  --relay https://localhost:4443 \\
  --tls-disable-verify \\
  -x $PUB_SUBS \\
  -y $TOTAL_SUBS \\
  -n $TOP_N \\
  -d $DURATION \\
  --group-interval-ms 33 \\
  --connection-batch-size 50 \\
  > $REMOTE_DIR/test_output.txt 2>&1 || true

# Capture memory after test
ps -o rss= -p \$RELAY_PID > $REMOTE_DIR/mem_after.txt 2>/dev/null || echo "0" > $REMOTE_DIR/mem_after.txt

wait \$PERF_PID 2>/dev/null || true

# Generate flamegraph
perf script -i $REMOTE_DIR/perf.data | ~/FlameGraph/stackcollapse-perf.pl > $REMOTE_DIR/collapsed.txt
~/FlameGraph/flamegraph.pl $REMOTE_DIR/collapsed.txt > $REMOTE_DIR/flamegraph.svg

# Run analysis
python3 ~/moq-rs-top-n/tools/analyze_flamegraph.py $REMOTE_DIR/collapsed.txt $REMOTE_DIR/analysis.txt

kill \$RELAY_PID 2>/dev/null || true
echo "DONE" > $REMOTE_DIR/status.txt
PERF_SCRIPT

echo "  Done."

# Step 4: Wait for completion and verify
echo "[4/7] Verifying completion..."
STATUS=$($SSH "cat $REMOTE_DIR/status.txt 2>/dev/null || echo FAILED")
if [ "$STATUS" != "DONE" ]; then
  echo "  ERROR: Test did not complete successfully"
  exit 1
fi
echo "  Done."

# Step 5: Collect results
echo "[5/7] Collecting results from remote..."
$SCP "$REMOTE:$REMOTE_DIR/flamegraph.svg" "$LOCAL_DIR/flamegraph.svg"
$SCP "$REMOTE:$REMOTE_DIR/collapsed.txt" "$LOCAL_DIR/collapsed.txt"
$SCP "$REMOTE:$REMOTE_DIR/analysis.txt" "$LOCAL_DIR/analysis.txt"
$SCP "$REMOTE:$REMOTE_DIR/test_output.txt" "$LOCAL_DIR/test_output.txt"
$SCP "$REMOTE:$REMOTE_DIR/relay.log" "$LOCAL_DIR/relay.log"
$SCP "$REMOTE:$REMOTE_DIR/mem_before.txt" "$LOCAL_DIR/mem_before.txt"
$SCP "$REMOTE:$REMOTE_DIR/mem_after.txt" "$LOCAL_DIR/mem_after.txt"
echo "  Done."

# Step 6: Extract test metrics from output
echo "[6/7] Extracting metrics..."
TEST_OUTPUT="$LOCAL_DIR/test_output.txt"

# Extract key metrics from test output
OBJECTS_SENT=$(grep -o "objects_sent\":[0-9]*" "$TEST_OUTPUT" | tail -1 | cut -d: -f2 || echo "N/A")
OBJECTS_RECV=$(grep -o "objects_received\":[0-9]*" "$TEST_OUTPUT" | tail -1 | cut -d: -f2 || echo "N/A")
FILTER_PASS=$(grep -c "TOPN_EVENT.*filter_pass" "$TEST_OUTPUT" 2>/dev/null || echo "0")
FILTER_DROP=$(grep -c "TOPN_EVENT.*filter_drop" "$TEST_OUTPUT" 2>/dev/null || echo "0")

# Memory
MEM_BEFORE=$(cat "$LOCAL_DIR/mem_before.txt" | tr -d ' ')
MEM_AFTER=$(cat "$LOCAL_DIR/mem_after.txt" | tr -d ' ')
MEM_BEFORE_MB=$(echo "scale=1; $MEM_BEFORE / 1024" | bc 2>/dev/null || echo "N/A")
MEM_AFTER_MB=$(echo "scale=1; $MEM_AFTER / 1024" | bc 2>/dev/null || echo "N/A")

# Extract from analysis
TOPN_SELF=$(grep "Total Top-N self time" "$LOCAL_DIR/analysis.txt" | grep -o "[0-9.]*%" | head -1 || echo "N/A")
TOPN_INCLUSIVE=$(grep "Top-N compute (inclusive)" "$LOCAL_DIR/analysis.txt" | grep -o "[0-9.]*%" | head -1 || echo "N/A")
QUIC_SELF=$(grep "QUIC transport (self)" "$LOCAL_DIR/analysis.txt" | grep -o "[0-9.]*%" | head -1 || echo "N/A")
QUIC_INCLUSIVE=$(grep "QUIC Transport" "$LOCAL_DIR/analysis.txt" | head -1 | grep -o "[0-9.]*%" | head -1 || echo "N/A")
MOQ_INCLUSIVE=$(grep "MOQ protocol (inclusive)" "$LOCAL_DIR/analysis.txt" | grep -o "[0-9.]*%" | head -1 || echo "N/A")
ALLOC_SELF=$(grep "Memory Allocation" "$LOCAL_DIR/analysis.txt" | tail -1 | grep -o "[0-9.]*%" | head -1 || echo "N/A")

echo "  Done."

# Step 7: Generate report
echo "[7/7] Generating report..."
REPORT="$LOCAL_DIR/report.md"
SYSINFO=$(cat "$LOCAL_DIR/sysinfo.txt")
ANALYSIS=$(cat "$LOCAL_DIR/analysis.txt")

cat > "$REPORT" <<EOF
# MOQ-RS Relay Performance Report

**Date:** $(date -u '+%Y-%m-%d %H:%M:%S UTC')
**Commit:** $(grep "Git commit:" "$LOCAL_DIR/sysinfo.txt" | cut -d: -f2- | xargs)
**Branch:** $(grep "Git branch:" "$LOCAL_DIR/sysinfo.txt" | cut -d: -f2- | xargs)

## Test Parameters

| Parameter | Value |
|-----------|-------|
| Publishers (pub-sub) | $PUB_SUBS |
| Pure subscribers | $PURE_SUBS |
| Total subscribers | $TOTAL_SUBS |
| Top-N filter | $TOP_N |
| Duration | ${DURATION}s |
| Group interval | 33ms (~30 fps) |
| Connection batch size | 50 |

## System Configuration

\`\`\`
$SYSINFO
\`\`\`

## Results Summary

### CPU Profile

| Component | Self Time | Inclusive Time |
|-----------|-----------|----------------|
| **Top-N** | $TOPN_SELF | $TOPN_INCLUSIVE |
| QUIC Transport | $QUIC_SELF | $QUIC_INCLUSIVE |
| MOQ Protocol | — | $MOQ_INCLUSIVE |
| Memory Allocation | $ALLOC_SELF | — |

### Memory Usage

| Metric | Value |
|--------|-------|
| RSS before test | ${MEM_BEFORE_MB} MB |
| RSS after test | ${MEM_AFTER_MB} MB |
| Growth | $(echo "scale=1; ($MEM_AFTER - $MEM_BEFORE) / 1024" | bc 2>/dev/null || echo "N/A") MB |

### Top-N vs QUIC Relative Cost

- Self vs self: Top-N $TOPN_SELF vs QUIC $QUIC_SELF
- Inclusive vs inclusive: Top-N $TOPN_INCLUSIVE vs QUIC $QUIC_INCLUSIVE

## Detailed Flamegraph Analysis

\`\`\`
$ANALYSIS
\`\`\`

## Artifacts

- \`flamegraph.svg\` — Interactive flamegraph
- \`collapsed.txt\` — Collapsed stacks for custom analysis
- \`test_output.txt\` — Full test driver output
- \`relay.log\` — Relay server logs
- \`analysis.txt\` — Raw flamegraph analysis

EOF

echo "  Done."
echo ""
echo "=== Report generated: $REPORT ==="
echo "=== Flamegraph:       $LOCAL_DIR/flamegraph.svg ==="
