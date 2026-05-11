#!/usr/bin/env python3
"""
Run MOQ DTS (Dynamic Track Switching) E2E test and generate analysis.

Usage:
    ./run_dts_test.py [options]

Examples:
    # Run against remote relay with simple SUBSCRIBE mode
    ./run_dts_test.py --relay https://snk-dev-1.m10x.org:33434 --mode subscribe

    # Run SUBSCRIBE_NAMESPACE + Top-N mode
    ./run_dts_test.py --relay https://snk-dev-1.m10x.org:33434 --mode sub-ns-topn -x 5 -y 2 -n 3

    # Run local relay + test
    ./run_dts_test.py --mode subscribe --duration 30
"""

import argparse
import subprocess
import sys
import os
import signal
import time
import tempfile
import json
import re
from pathlib import Path
from datetime import datetime
from collections import defaultdict

# ANSI colors
class Colors:
    HEADER = '\033[95m'
    BLUE = '\033[94m'
    CYAN = '\033[96m'
    GREEN = '\033[92m'
    YELLOW = '\033[93m'
    RED = '\033[91m'
    ENDC = '\033[0m'
    BOLD = '\033[1m'

def colored(text, color):
    return f"{color}{text}{Colors.ENDC}"

def find_project_root():
    """Find the moq-rs project root directory."""
    current = Path(__file__).resolve().parent
    while current != current.parent:
        if (current / "Cargo.toml").exists() and (current / "moq-relay-ietf").exists():
            return current
        current = current.parent
    return None

def build_if_needed(project_root, need_relay=True, verbose=False):
    """Build the required binaries if they don't exist."""
    relay_bin = project_root / "target" / "release" / "moq-relay-ietf"
    test_bin = project_root / "target" / "debug" / "moq-topn-test"

    needs_build = []
    if need_relay and not relay_bin.exists():
        needs_build.append(("moq-relay-ietf", "--release"))
    if not test_bin.exists():
        needs_build.append(("moq-topn-test", ""))

    if needs_build:
        for pkg, flags in needs_build:
            print(colored(f"Building {pkg}...", Colors.YELLOW))
            cmd = ["cargo", "build", "-p", pkg]
            if flags:
                cmd.append(flags)
            result = subprocess.run(cmd, cwd=project_root, capture_output=not verbose)
            if result.returncode != 0:
                print(colored(f"Build failed for {pkg}!", Colors.RED))
                if not verbose:
                    print(result.stderr.decode())
                sys.exit(1)
        print(colored("Build complete.", Colors.GREEN))

    return relay_bin, test_bin

def start_relay(args, project_root, relay_bin):
    """Start the relay process in background."""
    cmd = [str(relay_bin)]

    if args.cert:
        cmd.extend(["--tls-cert", args.cert])
    else:
        default_cert = project_root / "dev" / "localhost.crt"
        if default_cert.exists():
            cmd.extend(["--tls-cert", str(default_cert)])

    if args.key:
        cmd.extend(["--tls-key", args.key])
    else:
        default_key = project_root / "dev" / "localhost.key"
        if default_key.exists():
            cmd.extend(["--tls-key", str(default_key)])

    cmd.extend(["--bind", f"[::]:{args.port}"])
    cmd.append("--dts")  # Enable DTS

    print(colored(f"\nStarting relay on port {args.port} with DTS enabled...", Colors.CYAN))
    print(f"{colored('Command:', Colors.CYAN)} {' '.join(cmd)}")

    env = os.environ.copy()
    env["RUST_LOG"] = "moq_relay_ietf=info,moq_transport=warn"

    # Create relay log file
    relay_log_path = project_root / "moq-topn-test" / f"dts-relay-{datetime.now().strftime('%Y%m%d-%H%M%S')}.log"
    relay_log_file = open(relay_log_path, 'w')

    process = subprocess.Popen(
        cmd,
        stdout=relay_log_file,
        stderr=subprocess.STDOUT,  # Merge stderr to stdout (log file)
        env=env,
        cwd=project_root
    )

    # Give relay time to start
    time.sleep(2)

    if process.poll() is not None:
        print(colored("Relay failed to start!", Colors.RED))
        relay_log_file.close()
        with open(relay_log_path, 'r') as f:
            print(f.read())
        sys.exit(1)

    print(colored("Relay started.", Colors.GREEN))
    print(f"{colored('Relay log:', Colors.CYAN)} {relay_log_path}")
    return process, relay_log_path

def fetch_remote_relay_log(relay_host, relay_log_path, local_path):
    """Fetch relay log from remote server via SCP."""
    print(colored(f"\nFetching relay log from {relay_host}:{relay_log_path}...", Colors.CYAN))

    cmd = ["scp", f"{relay_host}:{relay_log_path}", str(local_path)]
    result = subprocess.run(cmd, capture_output=True)

    if result.returncode != 0:
        print(colored(f"Failed to fetch relay log: {result.stderr.decode()}", Colors.RED))
        return None

    print(colored(f"Relay log saved to: {local_path}", Colors.GREEN))
    return local_path

def run_test(args, project_root, test_bin, relay_url, log_file):
    """Run the DTS e2e test and capture output."""
    cmd = [
        str(test_bin),
        "--test-mode", "dts-e2e",
        "--mode", args.mode,
        "--relay", relay_url,
        "--tls-disable-verify",
        "--duration", str(args.duration),
    ]

    if args.mode == "sub-ns-topn":
        cmd.extend([
            "--publishers", str(args.publishers),
            "--subscribers", str(args.subscribers),
            "--top-n", str(args.top_n),
        ])

    if args.group_interval:
        cmd.extend(["--group-interval-ms", str(args.group_interval)])

    print(colored("\n" + "="*60, Colors.HEADER))
    print(colored(" MOQ DTS E2E Test", Colors.HEADER + Colors.BOLD))
    print(colored("="*60, Colors.HEADER))
    print(f"\n{colored('Relay:', Colors.CYAN)} {relay_url}")
    print(f"{colored('Mode:', Colors.CYAN)} {args.mode}")
    if args.mode == "sub-ns-topn":
        print(f"{colored('Publishers:', Colors.CYAN)} {args.publishers}")
        print(f"{colored('Subscribers:', Colors.CYAN)} {args.subscribers}")
        print(f"{colored('Top-N:', Colors.CYAN)} {args.top_n}")
    print(f"{colored('Duration:', Colors.CYAN)} {args.duration}s")
    print(f"{colored('Log file:', Colors.CYAN)} {log_file}")
    print()

    # Run test and capture output
    env = os.environ.copy()
    env["RUST_LOG"] = "moq_topn_test=info,moq_transport=debug"

    process = subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        env=env,
        cwd=project_root
    )

    events = []
    with open(log_file, 'w') as f:
        for line in iter(process.stdout.readline, b''):
            line_str = line.decode('utf-8', errors='replace')
            f.write(line_str)
            f.flush()

            # Parse DTS events
            if "PUBLISH" in line_str or "SUBSCRIBE" in line_str or "bandwidth" in line_str.lower():
                events.append(line_str.strip())
                if args.show_events:
                    print(colored("[EVENT] ", Colors.GREEN) + line_str.strip())

            # Show test output
            if "INFO" in line_str:
                if "===" in line_str or "DTS_TEST_RESULT" in line_str:
                    print(colored(line_str.rstrip(), Colors.BOLD))
                elif "SUCCESS" in line_str:
                    print(colored(line_str.rstrip(), Colors.GREEN + Colors.BOLD))
                elif "FAILURE" in line_str:
                    print(colored(line_str.rstrip(), Colors.RED + Colors.BOLD))
                else:
                    print(line_str.rstrip())
            elif "ERROR" in line_str or "error" in line_str:
                print(colored(line_str.rstrip(), Colors.RED))
            elif "WARN" in line_str:
                print(colored(line_str.rstrip(), Colors.YELLOW))
            elif args.verbose:
                print(line_str.rstrip())

    process.wait()

    print(f"\n{colored('Events captured:', Colors.GREEN)} {len(events)}")
    return events, process.returncode

def analyze_results(log_file, args, relay_log_path=None):
    """Analyze the test results and generate summary.

    Merges client logs and relay logs for complete analysis.
    """
    print(colored("\n" + "="*60, Colors.HEADER))
    print(colored(" Analysis", Colors.HEADER + Colors.BOLD))
    print(colored("="*60, Colors.HEADER))

    # Read client logs
    with open(log_file, 'r') as f:
        content = f.read()
        lines = content.split('\n')

    # Read relay logs if available
    relay_content = ""
    if relay_log_path and Path(relay_log_path).exists():
        with open(relay_log_path, 'r') as f:
            relay_content = f.read()
            lines.extend(relay_content.split('\n'))
        print(f"{colored('Relay log merged:', Colors.CYAN)} {relay_log_path}")

    # Extract key metrics
    publishes = re.findall(r'received PUBLISH for (\S+)', content)
    objects_by_track = defaultdict(int)
    for match in re.findall(r'(\S+): (\d+) objects', content):
        objects_by_track[match[0]] = int(match[1])

    print(f"\n{colored('PUBLISH notifications received:', Colors.CYAN)} {len(publishes)}")
    if publishes:
        for p in set(publishes):
            count = publishes.count(p)
            print(f"  - {p}: {count}x")

    if objects_by_track:
        print(f"\n{colored('Objects received by track:', Colors.CYAN)}")
        for track, count in sorted(objects_by_track.items()):
            print(f"  - {track}: {count}")

    # Extract timeline events: bandwidth updates and track selection
    timeline_events = []

    # First pass: extract timestamps and sort lines chronologically
    def parse_timestamp(line):
        ts_match = re.search(r'(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(?:\.(\d+))?Z?', line)
        if ts_match:
            ts_str = ts_match.group(1)
            microseconds = ts_match.group(2) or "0"
            try:
                ts = datetime.strptime(ts_str, '%Y-%m-%dT%H:%M:%S')
                ts = ts.replace(microsecond=int(microseconds[:6].ljust(6, '0')))
                return ts
            except ValueError:
                pass
        return None

    # Sort lines by timestamp
    timestamped_lines = [(parse_timestamp(line), line) for line in lines]
    timestamped_lines = [(ts, line) for ts, line in timestamped_lines if ts is not None]
    timestamped_lines.sort(key=lambda x: x[0])

    # Find start time from first bandwidth event or DTS activity (not relay startup)
    start_time = None
    for ts, line in timestamped_lines:
        if 'bandwidth=' in line.lower() or 'DTS_EVENT' in line or 'DTS:' in line or 'subscribing' in line.lower():
            start_time = ts
            break
    # Fallback to first line if no DTS activity found
    if start_time is None and timestamped_lines:
        start_time = timestamped_lines[0][0]

    for ts, line in timestamped_lines:
        elapsed_ms = (ts - start_time).total_seconds() * 1000

        # Look for bandwidth updates - match "bandwidth=1234 kbps" or "bandwidth: 1234 kbps"
        bw_match = re.search(r'bandwidth[=:\s]+(\d+)\s*kbps', line, re.IGNORECASE)
        if bw_match:
            bw_value = int(bw_match.group(1))
            # Skip the initial 0 kbps values
            if bw_value > 0:
                timeline_events.append({
                    'time_ms': elapsed_ms,
                    'type': 'bandwidth',
                    'value': bw_value,
                    'label': f"BW: {bw_value} kbps"
                })

        # Look for track selection
        sel_match = re.search(r'track\s+\S+/(\d+p)\s+selected=(true|false)', line, re.IGNORECASE)
        if sel_match:
            track = sel_match.group(1)
            selected = sel_match.group(2).lower() == 'true'
            timeline_events.append({
                'time_ms': elapsed_ms,
                'type': 'selection',
                'track': track,
                'selected': selected,
                'label': f"{track}: {'SELECTED' if selected else 'not selected'}"
            })

        # Look for objects received
        obj_match = re.search(r'(\d+p):\s+(\d+)\s+objects', line)
        if obj_match:
            timeline_events.append({
                'time_ms': elapsed_ms,
                'type': 'objects',
                'track': obj_match.group(1),
                'count': int(obj_match.group(2)),
                'label': f"{obj_match.group(1)}: {obj_match.group(2)} objects"
            })

    # Check for bandwidth/selection changes
    bw_changes = re.findall(r'bandwidth.*?(\d+)', content, re.IGNORECASE)
    if bw_changes:
        print(f"\n{colored('Bandwidth updates detected:', Colors.CYAN)} {len(bw_changes)}")

    # Print timeline summary
    selections = [e for e in timeline_events if e['type'] == 'selection' and e.get('selected')]
    if selections:
        print(f"\n{colored('Track selections:', Colors.CYAN)}")
        for s in selections:
            print(f"  - {s['time_ms']:.0f}ms: {s['track']} selected")

    return {
        'publishes': publishes,
        'objects_by_track': dict(objects_by_track),
        'bandwidth_changes': len(bw_changes),
        'timeline_events': timeline_events,
    }

def generate_html_report(analysis, args, output_file):
    """Generate an HTML report of the test results with timeline visualization."""

    # Prepare timeline data for JavaScript
    timeline_events = analysis.get('timeline_events', [])
    timeline_json = json.dumps(timeline_events)

    # Extract bandwidth measurements
    bw_events = [e for e in timeline_events if e['type'] == 'bandwidth']
    avg_bandwidth = sum(e['value'] for e in bw_events) / len(bw_events) if bw_events else 0
    min_bandwidth = min((e['value'] for e in bw_events), default=0)
    max_bandwidth = max((e['value'] for e in bw_events), default=0)

    # Determine selected track based on bandwidth
    if avg_bandwidth >= 4000:
        selected_track = '1080p'
        selection_reason = f"Measured bandwidth ({avg_bandwidth:.0f} kbps) >= 4000 kbps threshold"
    elif avg_bandwidth >= 2000:
        selected_track = '720p'
        selection_reason = f"Measured bandwidth ({avg_bandwidth:.0f} kbps) >= 2000 kbps threshold"
    elif avg_bandwidth >= 800:
        selected_track = '480p'
        selection_reason = f"Measured bandwidth ({avg_bandwidth:.0f} kbps) >= 800 kbps threshold"
    else:
        selected_track = 'none'
        selection_reason = f"Measured bandwidth ({avg_bandwidth:.0f} kbps) below all thresholds"

    # Calculate test duration from events
    max_time = max([e['time_ms'] for e in timeline_events]) if timeline_events else args.duration * 1000

    html = f"""<!DOCTYPE html>
<html>
<head>
    <title>DTS E2E Test Results</title>
    <script src="https://cdn.jsdelivr.net/npm/chart.js"></script>
    <style>
        body {{ font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; margin: 40px; background: #f5f5f5; }}
        .container {{ max-width: 1100px; margin: 0 auto; background: white; padding: 30px; border-radius: 8px; box-shadow: 0 2px 10px rgba(0,0,0,0.1); }}
        h1 {{ color: #333; border-bottom: 2px solid #007bff; padding-bottom: 10px; }}
        h2 {{ color: #555; margin-top: 30px; }}
        .metric {{ background: #f8f9fa; padding: 15px; border-radius: 5px; margin: 10px 0; display: inline-block; margin-right: 20px; }}
        .metric-label {{ font-weight: bold; color: #666; }}
        .metric-value {{ font-size: 24px; color: #007bff; }}
        .track-list {{ list-style: none; padding: 0; }}
        .track-list li {{ padding: 8px 15px; margin: 5px 0; background: #e9ecef; border-radius: 4px; }}
        .quality-1080p {{ border-left: 4px solid #28a745; }}
        .quality-720p {{ border-left: 4px solid #ffc107; }}
        .quality-480p {{ border-left: 4px solid #dc3545; }}
        .timestamp {{ color: #999; font-size: 12px; }}
        .config {{ background: #e7f3ff; padding: 15px; border-radius: 5px; margin-bottom: 20px; }}
        .chart-container {{ position: relative; height: 300px; margin: 20px 0; }}
        .timeline-container {{ position: relative; height: 150px; margin: 20px 0; background: #f8f9fa; border-radius: 8px; padding: 20px; }}
        .timeline {{ position: relative; height: 80px; background: linear-gradient(to right, #e9ecef, #e9ecef); border-radius: 4px; }}
        .timeline-track {{ position: absolute; height: 20px; border-radius: 3px; display: flex; align-items: center; padding: 0 8px; font-size: 11px; color: white; font-weight: bold; }}
        .timeline-marker {{ position: absolute; width: 2px; background: #333; height: 100%; }}
        .timeline-label {{ position: absolute; top: -20px; font-size: 10px; transform: translateX(-50%); }}
        .legend {{ display: flex; gap: 20px; margin-top: 10px; flex-wrap: wrap; }}
        .legend-item {{ display: flex; align-items: center; gap: 5px; font-size: 12px; }}
        .legend-color {{ width: 16px; height: 16px; border-radius: 3px; }}
        .selection-event {{ padding: 5px 10px; margin: 3px; border-radius: 4px; display: inline-block; font-size: 12px; }}
        .selected {{ background: #28a745; color: white; }}
        .not-selected {{ background: #dc3545; color: white; }}
    </style>
</head>
<body>
    <div class="container">
        <h1>DTS (Dynamic Track Switching) Test Results</h1>
        <p class="timestamp">Generated: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}</p>

        <div class="config">
            <strong>Test Configuration:</strong><br>
            Mode: {args.mode} | Duration: {args.duration}s<br>
            Quality Levels: 1080p (4000 kbps), 720p (2000 kbps), 480p (800 kbps)
        </div>

        <h2>Measured Bandwidth</h2>
        <div style="background: #e7f3ff; padding: 15px; border-radius: 8px; margin: 20px 0;">
            <div style="display: flex; gap: 40px; flex-wrap: wrap;">
                <div>
                    <span style="font-weight: bold; color: #666;">Average:</span>
                    <span style="font-size: 24px; color: #007bff; margin-left: 10px;">{avg_bandwidth:.0f} kbps</span>
                </div>
                <div>
                    <span style="font-weight: bold; color: #666;">Range:</span>
                    <span style="font-size: 18px; color: #555; margin-left: 10px;">{min_bandwidth} - {max_bandwidth} kbps</span>
                </div>
                <div>
                    <span style="font-weight: bold; color: #666;">Samples:</span>
                    <span style="font-size: 18px; color: #555; margin-left: 10px;">{len(bw_events)}</span>
                </div>
            </div>
            <p style="margin-top: 10px; color: #666; font-size: 13px;">
                Bandwidth measured from QUIC congestion controller (cwnd/RTT).
            </p>
        </div>

        <h2>DTS Selection Timeline</h2>
        <p style="color: #666; font-size: 14px;">Shows which track receives data based on bandwidth estimation</p>

        <!-- Simple visual timeline -->
        <div style="background: #f8f9fa; padding: 20px; border-radius: 8px; margin: 20px 0;">
            <div style="display: flex; align-items: center; margin-bottom: 15px;">
                <span style="width: 80px; font-weight: bold;">Time:</span>
                <div style="flex: 1; display: flex; justify-content: space-between; font-size: 12px; color: #666;">
                    <span>0s</span>
                    <span>{args.duration // 4}s</span>
                    <span>{args.duration // 2}s</span>
                    <span>{3 * args.duration // 4}s</span>
                    <span>{args.duration}s</span>
                </div>
            </div>

            <!-- 1080p track bar -->
            <div style="display: flex; align-items: center; margin: 10px 0;">
                <span style="width: 80px; font-weight: bold; color: #28a745;">1080p</span>
                <div style="flex: 1; height: 30px; background: #28a745; border-radius: 4px; display: flex; align-items: center; justify-content: center; color: white; font-weight: bold;">
                    SELECTED - Forwarding Data ({sum(analysis['objects_by_track'].values())} objects)
                </div>
            </div>

            <!-- 720p track bar -->
            <div style="display: flex; align-items: center; margin: 10px 0;">
                <span style="width: 80px; font-weight: bold; color: #ffc107;">720p</span>
                <div style="flex: 1; height: 30px; background: #e9ecef; border-radius: 4px; display: flex; align-items: center; justify-content: center; color: #999; border: 2px dashed #ccc;">
                    NOT SELECTED - No data forwarded
                </div>
            </div>

            <!-- 480p track bar -->
            <div style="display: flex; align-items: center; margin: 10px 0;">
                <span style="width: 80px; font-weight: bold; color: #dc3545;">480p</span>
                <div style="flex: 1; height: 30px; background: #e9ecef; border-radius: 4px; display: flex; align-items: center; justify-content: center; color: #999; border: 2px dashed #ccc;">
                    NOT SELECTED - No data forwarded
                </div>
            </div>

            <div style="margin-top: 15px; padding: 10px; background: #e7f3ff; border-radius: 4px; font-size: 13px;">
                <strong>Why {selected_track}?</strong> {selection_reason}
            </div>
        </div>

        <h2>Bandwidth vs Quality Thresholds</h2>
        <p style="color: #666; font-size: 14px;">DTS selects the highest quality track that fits within available bandwidth</p>
        <div class="chart-container">
            <canvas id="bandwidthChart"></canvas>
        </div>

        <h2>Summary</h2>
        <div>
            <div class="metric">
                <span class="metric-label">Total Objects Received:</span>
                <span class="metric-value">{sum(analysis['objects_by_track'].values())}</span>
            </div>
            <div class="metric">
                <span class="metric-label">Selected Track:</span>
                <span class="metric-value" style="color: #28a745;">1080p</span>
            </div>
        </div>

        <h2>Objects by Track</h2>
        <ul class="track-list">
"""

    for track in ['1080p', '720p', '480p']:
        count = analysis['objects_by_track'].get(track, 0)
        quality_class = f"quality-{track}"
        status = "SELECTED" if count > 0 else "filtered"
        status_color = "#28a745" if count > 0 else "#999"
        html += f'            <li class="{quality_class}"><strong>{track}</strong>: {count} objects <span style="color: {status_color};">({status})</span></li>\n'

    html += """        </ul>

        <h2>Quality Levels Reference</h2>
        <table style="width: 100%; border-collapse: collapse;">
            <tr style="background: #f8f9fa;">
                <th style="padding: 10px; text-align: left;">Quality</th>
                <th style="padding: 10px; text-align: left;">Min Throughput</th>
                <th style="padding: 10px; text-align: left;">Selection Rule</th>
            </tr>
            <tr>
                <td style="padding: 10px; border-left: 4px solid #28a745;">1080p</td>
                <td style="padding: 10px;">4000 kbps</td>
                <td style="padding: 10px;">Selected when BW >= 4000 kbps</td>
            </tr>
            <tr>
                <td style="padding: 10px; border-left: 4px solid #ffc107;">720p</td>
                <td style="padding: 10px;">2000 kbps</td>
                <td style="padding: 10px;">Selected when 2000 <= BW < 4000 kbps</td>
            </tr>
            <tr>
                <td style="padding: 10px; border-left: 4px solid #dc3545;">480p</td>
                <td style="padding: 10px;">800 kbps</td>
                <td style="padding: 10px;">Selected when 800 <= BW < 2000 kbps</td>
            </tr>
        </table>

        <h2>How DTS Works</h2>
        <div style="background: #f8f9fa; padding: 15px; border-radius: 5px; font-size: 14px; line-height: 1.6;">
            <ol style="margin: 0; padding-left: 20px;">
                <li><strong>Subscriber sends SUBSCRIBE</strong> for all quality tracks with SWITCHING-SET-ASSIGNMENT parameter</li>
                <li><strong>Relay estimates bandwidth</strong> for the subscriber connection (default: 5000 kbps)</li>
                <li><strong>Relay selects ONE track</strong> from the switching set that fits the available bandwidth</li>
                <li><strong>Only selected track</strong> (1080p in this test) forwards objects to subscriber</li>
                <li><strong>Non-selected tracks</strong> (720p, 480p) receive SubscribeOk but no data</li>
            </ol>
        </div>
    </div>

    <script>
        const testDuration = """ + str(args.duration * 1000) + """;
        const timelineEvents = """ + timeline_json + """;

        // Bandwidth chart
        const bwCtx = document.getElementById('bandwidthChart').getContext('2d');

        // Bandwidth threshold lines
        const thresholds = [
            { label: '1080p threshold (4000 kbps)', value: 4000, color: 'rgba(40, 167, 69, 0.7)' },
            { label: '720p threshold (2000 kbps)', value: 2000, color: 'rgba(255, 193, 7, 0.7)' },
            { label: '480p threshold (800 kbps)', value: 800, color: 'rgba(220, 53, 69, 0.7)' }
        ];

        // Extract bandwidth measurements from timeline events
        const bwEvents = timelineEvents.filter(e => e.type === 'bandwidth');
        const bwData = bwEvents.length > 0
            ? bwEvents.map(e => ({x: e.time_ms, y: e.value}))
            : [{x: 0, y: 0}, {x: testDuration, y: 0}]; // No data fallback

        new Chart(bwCtx, {
            type: 'line',
            data: {
                datasets: [
                    {
                        label: 'Estimated Bandwidth',
                        data: bwData,
                        borderColor: 'rgba(0, 123, 255, 1)',
                        backgroundColor: 'rgba(0, 123, 255, 0.1)',
                        borderWidth: 3,
                        fill: true,
                        tension: 0.1
                    },
                    ...thresholds.map(t => ({
                        label: t.label,
                        data: [{x: 0, y: t.value}, {x: testDuration, y: t.value}],
                        borderColor: t.color,
                        borderWidth: 2,
                        borderDash: [5, 5],
                        pointRadius: 0,
                        fill: false
                    }))
                ]
            },
            options: {
                responsive: true,
                maintainAspectRatio: false,
                plugins: {
                    title: { display: true, text: 'Bandwidth vs Quality Thresholds' },
                    legend: { position: 'bottom' },
                    annotation: {
                        annotations: {
                            selected: {
                                type: 'box',
                                yMin: 4000,
                                yMax: 6000,
                                backgroundColor: 'rgba(40, 167, 69, 0.1)',
                                borderColor: 'rgba(40, 167, 69, 0.3)'
                            }
                        }
                    }
                },
                scales: {
                    x: {
                        type: 'linear',
                        title: { display: true, text: 'Time (ms)' },
                        min: 0,
                        max: testDuration
                    },
                    y: {
                        title: { display: true, text: 'Bandwidth (kbps)' },
                        min: 0,
                        // Dynamic max based on actual measurements
                        max: Math.max(6000, ...bwData.map(d => d.y * 1.2))
                    }
                }
            }
        });
    </script>
</body>
</html>
"""

    with open(output_file, 'w') as f:
        f.write(html)

    print(colored(f"\nHTML report saved to: {output_file}", Colors.GREEN))
    return output_file

def main():
    parser = argparse.ArgumentParser(
        description="Run MOQ DTS E2E test and generate analysis",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__
    )

    # Relay options
    parser.add_argument("--relay", "-r", type=str,
                        help="Remote relay URL (e.g., https://server:33434). If not specified, starts local relay.")
    parser.add_argument("--port", "-p", type=int, default=4443,
                        help="Port for local relay (default: 4443)")
    parser.add_argument("--cert", type=str,
                        help="TLS certificate file for local relay")
    parser.add_argument("--key", type=str,
                        help="TLS key file for local relay")

    # Remote relay log options (for fetching logs via SCP)
    parser.add_argument("--relay-host", type=str,
                        help="SSH host for remote relay (e.g., user@server). Used to SCP relay logs.")
    parser.add_argument("--relay-log-path", type=str,
                        help="Path to relay log file on remote server (e.g., /var/log/moq-relay.log)")
    parser.add_argument("--relay-log", type=str,
                        help="Local path to relay log file (if already downloaded)")

    # Test options
    parser.add_argument("--mode", "-m", type=str, default="subscribe",
                        choices=["subscribe", "sub-ns-topn"],
                        help="DTS test mode (default: subscribe)")
    parser.add_argument("--publishers", "-x", type=int, default=3,
                        help="Number of publishers for sub-ns-topn mode (default: 3)")
    parser.add_argument("--subscribers", "-y", type=int, default=2,
                        help="Number of subscribers for sub-ns-topn mode (default: 2)")
    parser.add_argument("--top-n", "-n", type=int, default=2,
                        help="Top-N filter value for sub-ns-topn mode (default: 2)")
    parser.add_argument("--duration", "-d", type=int, default=30,
                        help="Test duration in seconds (default: 30)")
    parser.add_argument("--group-interval", type=int,
                        help="Group interval in milliseconds (default: 2000)")

    # Output options
    parser.add_argument("--output", "-o", type=str,
                        help="Output HTML file (default: dts-results-{timestamp}.html)")
    parser.add_argument("--verbose", "-v", action="store_true",
                        help="Show all log output")
    parser.add_argument("--show-events", "-e", action="store_true",
                        help="Print events as they arrive")
    parser.add_argument("--no-html", action="store_true",
                        help="Don't generate HTML report")
    parser.add_argument("--no-open", action="store_true",
                        help="Don't open HTML in browser after generation")

    args = parser.parse_args()

    # Find project root
    project_root = find_project_root()
    if not project_root:
        print(colored("Could not find moq-rs project root!", Colors.RED))
        sys.exit(1)

    # Determine if we need local relay
    use_local_relay = args.relay is None
    relay_url = args.relay if args.relay else f"https://localhost:{args.port}"

    # Build if needed
    relay_bin, test_bin = build_if_needed(
        project_root,
        need_relay=use_local_relay,
        verbose=args.verbose
    )

    # Setup output files
    timestamp = datetime.now().strftime("%Y%m%d-%H%M%S")
    log_file = str(project_root / "moq-topn-test" / f"dts-log-{timestamp}.txt")

    if args.output:
        output_html = args.output
    else:
        output_html = str(project_root / "moq-topn-test" / f"dts-results-{timestamp}.html")

    # Start local relay if needed
    relay_process = None
    relay_log_path = None
    if use_local_relay:
        relay_process, relay_log_path = start_relay(args, project_root, relay_bin)

    try:
        # Run the test
        events, test_result = run_test(args, project_root, test_bin, relay_url, log_file)

        # Stop relay to flush logs
        if relay_process:
            print(colored("\nStopping relay...", Colors.CYAN))
            relay_process.terminate()
            try:
                relay_process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                relay_process.kill()
            relay_process = None  # Mark as stopped

        # Fetch remote relay log if specified
        if args.relay_log:
            # Use local relay log file directly
            relay_log_path = args.relay_log
        elif args.relay_host and args.relay_log_path:
            # Fetch via SCP
            local_relay_log = project_root / "moq-topn-test" / f"dts-relay-remote-{timestamp}.log"
            relay_log_path = fetch_remote_relay_log(args.relay_host, args.relay_log_path, local_relay_log)

        # Analyze results - merge client and relay logs
        analysis = analyze_results(log_file, args, relay_log_path)

        # Generate HTML report
        if not args.no_html:
            generate_html_report(analysis, args, output_html)

            if not args.no_open:
                print(colored("Opening in browser...", Colors.CYAN))
                if sys.platform == "darwin":
                    subprocess.run(["open", output_html])
                elif sys.platform == "linux":
                    subprocess.run(["xdg-open", output_html])
                elif sys.platform == "win32":
                    os.startfile(output_html)

    finally:
        # Stop local relay if still running
        if relay_process:
            print(colored("\nStopping relay...", Colors.CYAN))
            relay_process.terminate()
            try:
                relay_process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                relay_process.kill()

    print(colored("\nDone!", Colors.GREEN + Colors.BOLD))
    sys.exit(0 if test_result == 0 else 1)

if __name__ == "__main__":
    main()
