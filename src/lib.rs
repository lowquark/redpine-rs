/*

https://intronetworks.cs.luc.edu/current/html/reno.html

UFlow makes a distinction between packets and frames.

# Congestion window

UFlow uses a rolling window to respond to congestion in a manner similar to TCP Reno. As a window of
packets is finished, the frame window sized (cwnd) is adjusted to reflect whether losses were
encountered in that window.

In slow start, each ack increases cwnd by:

  cwnd <- cwnd + 1/cwnd

In congestion avoidance, each ack increases cwnd by:

  cwnd <- cwnd + 1

Each time the current congestion window elapses, if any losses were present in the window, cwnd is
decreased by:

  cwnd <- cwnd / 2

The congestion window is said to elapse when an ack is received that is beyond the current window.

Whenever a timeout occurs (no ACK received after 1 RTO), cwnd is reset to:

  cwnd <- 10

A value of 10 (10 * 1460 bytes) is common on the modern (post 2013) internet.

Nothing in the congestion window is resent. Instead, the frame window is always advanced to one past
the most recently acknowledged frame. Data frames dropped during such an event can be assumed to no
longer be in transit, and so this scheme should not interfere with proper ACK clocking.

Data is sent in response to a received ACK or a flush operation. Data is pulled from the reliable
and unreliable queues subject to their individual bucket counters. Data is pulled from the
unreliable queue first.

The reliable and unreliable queues are virtual queues with their own fragment windows. The reliable
queue will return to fragment N when 3 duplicate ACKs for fragment N-1 have been received, as if
that packet was at the front of the queue. The unreliable queue will send packets subject to an
expiration time. If the next packet in the queue has expired, it is skipped as if it was not there.

# Reliable queue selective resend

Say reliable fragments are selectively acknowledged using 32 bits of information. Then, when
resending, the sender would be able to skip resending fragments which were not dropped.

It could be that more than 32 fragments in a row are dropped. If an ACK is sent in response to each
frame received by the receiver, and this ACK contains the 32 most recent fragment IDs received, the
sender would not know beyond those 32 fragment IDs which fragments were actually delivered, and may
unnecessarily resend those which were. This is most likely when a single frame containing many
reliable fragments is dropped, but resending the fragments contained by a single frame will have
little overall impact. On the other hand, a large packet split into many frame-filling fragments
would risk the most resent data, but dropping more than 32 frames in a row is seen as a very rare
event. In any case, resend performance of the reliable queue is worse without the selective ACK
mechanism, and having 32 bits of history should accomodate most dropped frames.

# Selective acking of reliable and unreliable queues

The reliable queue is only acked when the receiver receives a datagram containing one or more
reliable fragments, and the same goes for the unreliable queue. This effectively disambiguates
multiple acknowledgements due to missing reliable fragments, and multiple acknowledgements of the
same next reliable fragment ID for frames containing only unreliable data.

# Reordering mechanism

Frame data is reordered, and drops are detected, through a reorder buffer.

Case 0:

v              v
a _ _ _ _ => _ _ _ _ _  deliver a

Case 1:

v            v
_ b c _ _ => _ b c _ _

v                  v
a b c _ _ => _ _ _ _ _  deliver a, b, c

Case 2:

v                    v
_ b c d _ => _ _ _ _ _  drop a, deliver b, c, d

When a frame is reported dropped, and a drop counter is not zero, cwnd is halved as above. Then, the
drop counter is set equal to cwnd and subtracted from as frames are sent. This prevents multiple
drops from halving the window more than once per RTT.

An acknowledgement is always sent for any received frame that contains data. That data, however,
will be delivered subject to the reordering above. (TODO: Does a timeout flush this buffer?)

# Handshake

The handshake used by UFlow follows the classic SYN, SYN+ACK, ACK procedure used by TCP. A
SipHash MAC is used to alleviate the server from having to track pending client connections in
memory.

In addition to indicating its protocol version, each endpoint declares the maximum packet size that
it will send, and a maximum packet size that it will accept. These parameters serve to determine
whether a conneciton is feasible.

During the handshake, both the client and server request a maximum incoming bandwidth of the other.
This parameter is intended to slow reliable traffic and discard unreliable traffic to reflect
network limits known to the user. Some applications may inherently send at a high rate, so it is
recommended that the application set a minimum for this parameter in order to avoid the situation
where a malicious client requests a very low inbound bandwidth that would create excessive
buffering at the server.

# Throttling algorithm

#UFlow uses a preemptive leaky bucket algorithm to enforce a maximum send rate. That is, if the
#counter is greater than or equal to zero, a frame may be sent, and its size subtracted from the
#counter. At a later time, tokens will be added to the bucket, and more frames may be sent again.

UFlow places a maximum on cwnd which is updated as RTT estimates are computed. That is:

  cwnd_max <- RTT * max_bandwidth

# Traffic balancing algorithm

Data from both the reliable and unreliable queues are sent in a balanced manner according to a send
ratio assigned by a given endpoint. The endpoint keeps a signed counter and assigns a natural number
multiplier to each queue. If the current counter value is positive, data will be sent from the
reliable queue (provided there is a fragment ready to be sent), and if the current counter value is
negative, fragments will be taken from the unreliable queue instead.

Whenever fragments are taken from the reliable queue, the counter is updated by:

  bcnt <- bcnt - urel_factor * fragment_size

Likewise, when fragments are taken from the unreliable queue, the counter is updated by:

  bcnt <- bcnt + rel_factor * fragment_size

It can be seen that when a sufficient amount of data is present in both streams, the ratio of
reliable to unreliable data sent will approach rel_factor : urel_factor. The example below shows
this effect for fragment sizes, for rel_factor = 1 and urel_factor = 5.

  bcnt  delta  send
    0    -(5)   R
   -5    +(1)   U
   -4    +(1)   U
   -3    +(1)   U
   -2    +(1)   U
   -1    +(1)   U
    0    -(5)   R
   -5    +(1)   U
        ...

The counter saturates at user-defined positive and negative values.

*/

mod buffer;
pub mod client;
mod endpoint;
mod frame;
pub mod server;
mod socket;
mod timer_wheel;

pub enum SendMode {
    Reliable,
    Unreliable(u16),
}

pub type Client = client::Client;
pub type Server = server::Server;
