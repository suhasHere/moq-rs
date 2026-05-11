# DTS (Dynamic Track Switching) Testing Guide

This guide covers testing the DTS implementation in moq-rs, including simulation tests and integration with moq-web clients.

## Overview

DTS (Dynamic Track Switching) enables relays to dynamically select which track to forward from a **switching set** based on available downstream bandwidth. It's designed for ABR (Adaptive Bitrate) streaming scenarios where multiple quality renditions exist for the same content.

## Running Simulation Tests

The DTS simulation test validates the core switching logic without network involvement:

```bash
# Run DTS simulation tests
cargo run -p moq-topn-test --bin moq-topn-test -- --mode dts

# With verbose logging
cargo run -p moq-topn-test --bin moq-topn-test -- --mode dts --verbose
```

### What the Simulation Tests Cover

1. **Single Switching Set ABR**: Tests track selection based on bandwidth thresholds
2. **Multiple Sets with Rank Priority**: Tests allocation with different priority levels
3. **Group-Boundary Switching**: Tests that switches happen at group boundaries
4. **Bandwidth Dynamics**: Tests response to bandwidth changes

## Testing with moq-relay-ietf

### Starting the Relay

```bash
# Generate test certificates (if needed)
cargo run -p moq-relay-ietf --bin moq-relay-ietf -- \
    --bind 0.0.0.0:4443 \
    --tls-cert cert.pem \
    --tls-key key.pem
```

### Publishing Multi-Rendition Content

Use ffmpeg to create multiple quality renditions:

```bash
# Terminal 1: Publish 1080p
ffmpeg -re -f lavfi -i testsrc2=size=1920x1080:rate=30 \
    -c:v libx264 -preset ultrafast -b:v 4000k -f mp4 - | \
    cargo run -p moq-pub -- \
    --relay https://localhost:4443 \
    --namespace "video" \
    --track "1080p"

# Terminal 2: Publish 720p  
ffmpeg -re -f lavfi -i testsrc2=size=1280x720:rate=30 \
    -c:v libx264 -preset ultrafast -b:v 2000k -f mp4 - | \
    cargo run -p moq-pub -- \
    --relay https://localhost:4443 \
    --namespace "video" \
    --track "720p"

# Terminal 3: Publish 480p
ffmpeg -re -f lavfi -i testsrc2=size=854x480:rate=30 \
    -c:v libx264 -preset ultrafast -b:v 800k -f mp4 - | \
    cargo run -p moq-pub -- \
    --relay https://localhost:4443 \
    --namespace "video" \
    --track "480p"
```

## Testing with moq-web Clients

### Required moq-web Changes

For moq-web clients to use DTS, they need to:

1. **Encode SWITCHING-SET-ASSIGNMENT** in SUBSCRIBE messages:
   ```javascript
   // Parameter type 0x41 (odd = bytes value)
   const assignment = {
     setId: 1,
     throughputKbps: 3000,  // For 1080p track
     fraction: 10,
     activate: false,
     rank: 1
   };
   
   // Encode to bytes and add to SUBSCRIBE params
   subscribe.params.set(0x41, encodeAssignment(assignment));
   ```

2. **Handle track switching**: Be prepared to receive objects from different tracks within a switching set as bandwidth changes.

3. **Send all rendition SUBSCRIBEs**: Subscribe to all quality levels with appropriate `activate` flags:
   - Set `activate=0` for all tracks except the last
   - Set `activate=1` on the final track to signal the set is complete

### Example Subscribe Sequence

```javascript
// Subscribe to 1080p (don't activate yet)
await subscribe("video", "1080p", {
  switchingSet: { setId: 1, throughput: 3000, fraction: 10, activate: false }
});

// Subscribe to 720p (don't activate yet)
await subscribe("video", "720p", {
  switchingSet: { setId: 1, throughput: 1500, fraction: 10, activate: false }
});

// Subscribe to 480p (activate the set)
await subscribe("video", "480p", {
  switchingSet: { setId: 1, throughput: 800, fraction: 10, activate: true }
});

// Now relay will start forwarding based on available bandwidth
```

## Bandwidth Throttling for Testing

### Browser DevTools

1. Open Chrome DevTools → Network tab
2. Use the throttling dropdown to simulate:
   - "Slow 3G" (~400 kbps)
   - "Fast 3G" (~1.4 Mbps)
   - Custom presets

### Linux Traffic Control

```bash
# Add bandwidth limit
sudo tc qdisc add dev lo root tbf rate 2mbit burst 32kbit latency 50ms

# Remove limit
sudo tc qdisc del dev lo root
```

### macOS Network Link Conditioner

1. Install from Additional Tools for Xcode
2. System Preferences → Network Link Conditioner
3. Create custom profile with bandwidth limit

## Wire Protocol Details

### SWITCHING-SET-ASSIGNMENT Parameter (0x41)

```
Parameter Type: 0x41 (odd = BytesValue)
Encoding:
  Set ID         (varint)
  Throughput     (varint) - kbps
  Fraction       (varint) - relative weight 1-10
  Flags          (1 byte) - bit 0 = activate, bit 1 = rank present
  [Rank]         (1 byte) - optional, 1-255, lower = higher priority
```

### Example Encoding

For a 1080p track at 3000 kbps in set 1 with fraction 6 and rank 1:
```
Set ID:      0x01
Throughput:  0x0B 0xB8  (3000 in varint)
Fraction:    0x06
Flags:       0x03       (activate=1, rank_present=1)
Rank:        0x01
```

## Debugging

### Enable DTS Logging

```bash
# In relay
RUST_LOG=moq_relay_ietf::dts=debug cargo run -p moq-relay-ietf ...

# In tests
cargo run -p moq-topn-test --bin moq-topn-test -- --mode dts --verbose
```

### Common Issues

1. **Tracks not switching**: Check that `activate=true` was sent on the last track
2. **Wrong track selected**: Verify throughput thresholds are correct (in kbps)
3. **No tracks selected**: Bandwidth may be below all thresholds

## Architecture

```
┌─────────────────┐     ┌──────────────────┐     ┌─────────────────┐
│   Publisher     │     │   moq-relay      │     │   Subscriber    │
│                 │     │                  │     │                 │
│  ┌───────────┐  │     │  ┌────────────┐  │     │                 │
│  │   1080p   │──┼────►│  │            │  │     │                 │
│  └───────────┘  │     │  │   DTS      │  │     │                 │
│  ┌───────────┐  │     │  │  Tracker   │──┼────►│  Selected track │
│  │   720p    │──┼────►│  │            │  │     │                 │
│  └───────────┘  │     │  │ + Bandwidth│  │     │                 │
│  ┌───────────┐  │     │  │  Estimator │  │     │                 │
│  │   480p    │──┼────►│  │            │  │     │                 │
│  └───────────┘  │     │  └────────────┘  │     │                 │
└─────────────────┘     └──────────────────┘     └─────────────────┘
```

The relay:
1. Receives tracks from publisher
2. Monitors bandwidth to each subscriber
3. Selects appropriate track from switching set
4. Switches at group boundaries for smooth transitions

## References

- [DTS4MoQ Draft Spec](https://github.com/wilaw/dts4moq/blob/main/draft-wilaw-moq-dts4moq.md)
- [MoQ Transport Draft](https://datatracker.ietf.org/doc/draft-ietf-moq-transport/)
