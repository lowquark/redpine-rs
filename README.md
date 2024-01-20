![decoration.png](./decoration.png)

# redpine-rs

Redpine is a connection-oriented UDP messaging library for real-time internet
applications. It supports the delivery of both reliable and unreliable
datagrams, and allows the application to mitigate the undesirable effects of
network losses.

While the underlying protocol has been developed from scratch, Redpine has an
event-driven API based on the venerable
[ENet](https://github.com/lsalzman/enet), and was written based on lessons
learned during the development of its predecessor,
[UFlow](https://github.com/lowquark/uflow). In particular, Redpine makes four
key improvements to UFlow:

### Event-driven, optionally-blocking implementation

Users no longer need to continually call `service(...)` to exchange data, and
the server never iterates over the list of connected clients. Instead, Redpine
generates API events by scheduling timers and automatically waking while
blocking on the internal socket. A polling-based call which does not block is
still provided.

### Simplified congestion control

Instead of a math-heavy TFRC implementation, a lightweight congestion avoidance
algorithm that mirrors TCP Reno is used. This and the previous point should
increase the number of simultaneous connections that a server can handle, and
will likely improve TCP-friendliness overall. An algorithm based on TCP CUBIC
will be considered in the future.

### Simplified packet streaming model

Redpine does away with having many virtual packet channels that each support a
mixture of send modes. Instead, packet fragments are sent in a round-robin
fashion from one of two send queues: a queue containing reliable packet data,
and another containing unreliable packet data. For packets in the unreliable
queue, the user specifies how long a packet has before it expires.

It is thought that this model is sufficient for the majority of real-time
internet applications. However, channel-oriented methods for mitigating
head-of-line blocking in the reliable queue are still under consideration.
(Frankly, I couldn't come up with a use case that rationalized the complexity
of the previous sequencing model, considering some amount of application-level
reordering will always be required.)

### 4-way, SipHash-validated handshake

When a server receives an initial handshake from a client, the server does not
allocate any resources, but instead replies with the data it would have saved,
signed with a SipHash MAC. The connection will be established once the client
repeats this data + MAC, proving to the server that a previous handshake frame
was sent, and that the client is at the address that it claims to be. By
preventing address spoofing this way, Redpine is much more resilient to DDoS
attacks.

# TODO

  * Implement Nagle's algorithm

  * Figure out data-limited congestion-avoidance

  * Document

  * ???

  * Publish
