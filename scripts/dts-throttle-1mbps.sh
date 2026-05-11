#!/bin/bash
# Throttle to 1.2 Mbps - forces 480p selection
# 720p threshold: 2000 kbps (won't fit)
# 480p threshold: 500 kbps (fits with headroom)

IFACE="${1:-ens34}"

echo "Clearing existing tc rules..."
sudo tc qdisc del dev $IFACE root 2>/dev/null

echo "Throttling to 1200 kbps with large burst buffer..."
sudo tc qdisc add dev $IFACE root handle 1: htb default 10
sudo tc class add dev $IFACE parent 1: classid 1:10 htb rate 1200kbit ceil 1200kbit burst 200k cburst 200k

echo "Done. DTS should switch to 480p."
echo "To clear: sudo tc qdisc del dev $IFACE root"
