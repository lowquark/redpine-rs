![decoration.png](./decoration.png)

# redpine

`redpine` is a connection-oriented UDP messaging library for real-time internet
applications. It supports the delivery of both reliable and unreliable
datagrams, allowing the application to mitigate the negative effects of network
losses.

While the underlying protocol has been developed from scratch, `redpine` has an
event-driven API based on the venerable `enet` library, and was written based
on lessons learned during the development of its predecessor, `uflow`. In
particular, `redpine` makes four key improvements:

### Event-driven, optionally-blocking implementation

Users no longer need to periodically call `service(...)` to ensure data is
exchanged, and the server never iterates over the list of connected clients.
Now, `redpine` generates API events by scheduling timers and waking while
polling on the internal socket. A polling-based call which does not block is
still provided.

### Simplified packet streaming model

`redpine` does away with having many virtual packet channels that each support
a mixture of send modes. Instead, packet fragments are sent in a round-robin
fashion from one of two send queues: a queue containing reliable packet data,
and the other containing unreliable packet data. For the unreliable queue, the
user specifies how long a packet has before it expires.

It is thought that this model is sufficient for the majority of real-time
internet applications. (Frankly, I couldn't come up with a use case that
rationalized the complexity of the previous sequencing model.) However,
channel-oriented methods for resolving head-of-line blocking in the reliable
queue are still under consideration.

### 4-way, SipHash-validated handshake

When a server receives an initial handshake from a client, the server does not
allocate any resources, but instead appends a SipHash MAC to its response. The
connection will be established once the client repeats this MAC, proving to the
server that the client is at the address that it claims to be. By preventing
address spoofing this way, `redpine` is much more resilient to DDoS attacks,
and addresses may be whitelisted or blacklisted as appropriate.

### Simplified congestion control

Instead of a math-heavy TFRC implementation, a lightweight congestion avoidance
algorithm that mirrors TCP Reno is used. This and the previous point should
increase the number of simultaneous connections that a server can handle, and
will likely improve TCP-friendliness overall. CUBIC will be considered in the
future.

# TODO

  * Use CSPRNG when initializing SipHash key

  * Implement Nagle's algorithm

  * Figure out data-limited congestion-avoidance

  * ???

  * Publish
