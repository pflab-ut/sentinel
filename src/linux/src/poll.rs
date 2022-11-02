pub const POLL_READABLE_EVENTS: u64 = (libc::POLLIN | libc::POLLRDNORM) as u64;
pub const POLL_WRITABLE_EVENTS: u64 = (libc::POLLOUT | libc::POLLWRNORM) as u64;
pub const POLL_ALL_EVENTS: u64 = (0x1f | libc::POLLRDNORM | libc::POLLWRNORM) as u64;
