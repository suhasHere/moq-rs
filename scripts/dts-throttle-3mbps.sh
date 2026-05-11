#!/bin/bash
# Throttle to 3 Mbps - allows 480p, blocks 1080p
# 1080p threshold: 4000 kbps (won't fit)
# 480p threshold: 800 kbps (should work with headroom)

IFACE="${1:-ens34}"

echo "Clearing existing tc rules..."
sudo tc qdisc del dev $IFACE root 2>/dev/null

echo "Throttling to 3000 kbps with burst buffer..."
sudo tc qdisc add dev $IFACE root handle 1: htb default 10
sudo tc class add dev $IFACE parent 1: classid 1:10 htb rate 3000kbit ceil 3000kbit burst 100k cburst 100k
sudo tc qdisc add dev $IFACE parent 1:10 handle 10: netem delay 20ms

echo "Done. DTS should switch to 480p."
echo "To clear: sudo tc qdisc del dev $IFACE root"
