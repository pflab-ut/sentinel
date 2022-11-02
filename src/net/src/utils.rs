use std::io;

pub fn get_poll_event_from_fd(fd: i32, mask: u64) -> u64 {
    let mut pfd = libc::pollfd {
        fd,
        events: mask as i16,
        revents: 0,
    };
    let ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    loop {
        let n = unsafe { libc::ppoll(&mut pfd as *mut _, 1, &ts as *const _, std::ptr::null()) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            // Just say the fd is ready in case of an error.
            return mask;
        }
        if n == 0 {
            return 0;
        }
        return (pfd.revents as u64) & linux::POLL_ALL_EVENTS;
    }
}
