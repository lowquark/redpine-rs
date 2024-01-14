# The `uflow` 1.0 rewrite

This branch is a from-scratch rewrite of `uflow`, with the following improvements.

### Event-driven, optionally-blocking implementation

Users no longer need to repeatedly call `service(...)` just to exchange data,
and the server never iterates over the list of connected clients. Instead,
`uflow` schedules timers and self-wakes while polling on the internal socket.
(A non-blocking call still exists.)

### Simplified congestion control

Instead of a math-heavy TFRC implementation, a lightweight algorithm which
closely corresponds to TCP Reno is used. This and the previous point should
increase the number of simultaneous connections a server can handle, and will
likely improve TCP-friendliness overall. TCP-Cubic may be considered in the
future.

### 4-way, MAC-validated handshake

Servers allocate no resources whatsoever until clients respond with a
server-generated message authentication code. This effectively prevents address
spoofing, and makes `uflow` much more resilient to DDoS attacks.

### Simplified packet streaming model

Packet fragments from a reliable stream and unreliable stream are sent in a
bandwidth-prioritized fashion. Instead of having specialized modes for
unreliable packets, users may specify when an unreliable packet should expire,
causing that that packet to be skipped if too much time has been spent in the
local send queue. (Channel-oriented methods for resolving head-of-line blocking
in the reliable queue are still under consideration.)

# TODO

  * Pad initial frame to MTU

  * Use CSPRNG when initializing SipHash key

  * Implement Nagle's algorithm

  * Figure out data-limited congestion-avoidance

  * ???

  * Publish
