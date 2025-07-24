# 0.3.0

  - Resolved a critical bug with timer scheduling
  - Replaced the server timer wheel system with a simple and performant rotating buffer
  - Enabled server peers to send packets concurrently as the server waits for events
  - Replaced numeric peer IDs with arbitrary user data

# 0.2.1

  - Added the ability for PeerHandles to be cloned and compared

# 0.2.0

  - Added configuration options for unreliable / reliable channel prioritization
  - Replaced usage of Rc with Arc in order to support Send trait for Client and Server objects

# 0.1.0

  - Initial release
