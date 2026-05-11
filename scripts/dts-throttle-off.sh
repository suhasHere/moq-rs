#!/bin/bash
# Remove throttling - back to full bandwidth (1080p)

IFACE="${1:-ens34}"

echo "Removing throttle..."
sudo tc qdisc del dev $IFACE root 2>/dev/null

echo "Done. DTS should switch to 1080p."
