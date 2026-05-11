#!/bin/bash
# Throttle to 2.5 Mbps - forces 720p selection
# 1080p threshold: 4000 kbps (won't fit)
# 720p threshold: 2000 kbps (fits with headroom)
# 480p threshold: 500 kbps (fits easily)

IFACE="${1:-ens34}"

echo "Clearing existing tc rules..."
sudo tc qdisc del dev $IFACE root 2>/dev/null

echo "Throttling to 2500 kbps with burst buffer..."
sudo tc qdisc add dev $IFACE root handle 1: htb default 10
sudo tc class add dev $IFACE parent 1: classid 1:10 htb rate 2500kbit ceil 2500kbit burst 100k cburst 100k
sudo tc qdisc add dev $IFACE parent 1:10 handle 10: netem delay 20ms

echo "Done. DTS should switch to 720p."
echo "To clear: sudo tc qdisc del dev $IFACE root"
