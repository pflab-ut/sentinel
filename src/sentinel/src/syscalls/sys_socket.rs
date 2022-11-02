use std::{cell::RefCell, rc::Rc};

use fs::{
    socket::{build_socket_file, SocketFile},
    SettableFileFlags,
};
use mem::{Addr, IoOpts};
use utils::{bail_libc, SysError, SysResult};

use crate::{context, kernel::task::Task};

// socket implements linux syscall socket(2)
pub fn socket(regs: &libc::user_regs_struct) -> super::Result {
    let domain = regs.rdi as i32;
    let stype = regs.rsi as i32;
    let protocol = regs.rdx as i32;

    if stype & !(0xf | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC) != 0 {
        bail_libc!(libc::EINVAL);
    }

    let ctx = context::context();
    let mut socket = build_socket_file(domain, stype & 0xf, protocol, &*ctx)?;
    socket.set_flags(SettableFileFlags {
        direct: false,
        non_blocking: stype & libc::SOCK_NONBLOCK != 0,
        append: false,
        async_: false,
    });
    let socket = Rc::new(RefCell::new(socket));

    let mut task = ctx.task_mut();
    task.new_fd_from(
        0,
        &socket,
        fs::FdFlags {
            close_on_exec: stype & libc::SOCK_CLOEXEC != 0,
        },
    )
    .map(|n| n as usize)
}

const MAX_SOCKET_ADDR_LEN: u32 = 200;
fn copy_in_address(task: &Task, addr: Addr, addr_len: u32) -> SysResult<Vec<u8>> {
    if addr_len > MAX_SOCKET_ADDR_LEN {
        bail_libc!(libc::EINVAL);
    }
    let mut addr_buf = vec![0; addr_len as usize];
    task.copy_in_bytes(addr, &mut addr_buf)?;
    Ok(addr_buf)
}

// connect implements linux syscall connect(2)
pub fn connect(regs: &libc::user_regs_struct) -> super::Result {
    let fd = regs.rdi as i32;
    let addr = Addr(regs.rsi);
    let addrlen = regs.rdx as u32;

    let ctx = context::context();
    let mut task = ctx.task_mut();
    let file = task
        .get_file(fd)
        .ok_or_else(|| SysError::new(libc::EBADF))?;
    let is_blocking = !file.borrow().flags().non_blocking;
    let mut file = file.borrow_mut();
    let socket = file
        .file_operations_mut::<SocketFile>()
        .ok_or_else(|| SysError::new(libc::ENOTSOCK))?;
    let addr = copy_in_address(&*task, addr, addrlen)?;
    socket.connect(&addr, is_blocking, &*ctx).map(|()| 0)
}

// bind implements linux syscall bind(2)
pub fn bind(regs: &libc::user_regs_struct) -> super::Result {
    let fd = regs.rdi as i32;
    let addr = Addr(regs.rsi);
    let addrlen = regs.rdx as u32;

    let ctx = context::context();
    let file = {
        let mut task = ctx.task_mut();
        task.get_file(fd).ok_or_else(|| SysError::new(libc::EBADF))
    }?;
    let mut file = file.borrow_mut();
    let socket = file
        .file_operations_mut::<SocketFile>()
        .ok_or_else(|| SysError::new(libc::ENOTSOCK))?;
    let task = ctx.task();
    let addr = copy_in_address(&*task, addr, addrlen)?;
    socket.bind(&addr, &*ctx).map(|()| 0)
}

// listen implements linux syscall listen(2)
pub fn listen(regs: &libc::user_regs_struct) -> super::Result {
    let sockfd = regs.rdi as i32;
    let backlog = regs.rsi as i32;

    let ctx = context::context();
    let file = {
        let mut task = ctx.task_mut();
        task.get_file(sockfd)
            .ok_or_else(|| SysError::new(libc::EBADF))
    }?;
    let mut file = file.borrow_mut();
    let socket = file
        .file_operations_mut::<SocketFile>()
        .ok_or_else(|| SysError::new(libc::ENOTSOCK))?;
    socket.listen(backlog, &*ctx).map(|()| 0)
}

// sendto implements linux syscall sendto(2)
pub fn sendto(regs: &libc::user_regs_struct) -> super::Result {
    let sockfd = regs.rdi as i32;
    let buf_addr = Addr(regs.rsi);
    let buf_len = regs.rdx;
    let flags = regs.r10 as i32;
    let dest_addr = Addr(regs.r8);
    let dest_len = regs.r9 as u32;
    send_to(sockfd, buf_addr, buf_len, flags, dest_addr, dest_len)
}

fn send_to(
    sockfd: i32,
    buf_addr: Addr,
    buf_len: u64,
    mut flags: i32,
    dest_addr: Addr,
    dest_len: u32,
) -> SysResult<usize> {
    let buf_len = buf_len as i32;
    if buf_len < 0 {
        bail_libc!(libc::EINVAL);
    }
    let ctx = context::context();
    let file = {
        let mut task = ctx.task_mut();
        task.get_file(sockfd)
            .ok_or_else(|| SysError::new(libc::EBADF))?
    };
    let file = file.borrow();
    let socket = file
        .file_operations::<SocketFile>()
        .ok_or_else(|| SysError::new(libc::ENOTSOCK))?;
    if file.flags().non_blocking {
        flags |= libc::MSG_DONTWAIT;
    }
    let task = ctx.task();
    let dest = if dest_addr.0 != 0 {
        Some(copy_in_address(&*task, dest_addr, dest_len as u32)?)
    } else {
        None
    };
    let mut src = task.single_io_sequence(
        buf_addr,
        buf_len as i32,
        IoOpts {
            ignore_permissions: false,
        },
    )?;
    socket.send_msg(&mut src, dest.as_deref(), flags, &*ctx)
}

// recvfrom implements linux syscall recvfrom(2)
pub fn recvfrom(regs: &libc::user_regs_struct) -> super::Result {
    let sockfd = regs.rdi as i32;
    let buf = Addr(regs.rsi);
    let len = regs.rdx as usize;
    let flags = regs.r10 as i32;
    let src_addr = Addr(regs.r8);
    let addrlen = Addr(regs.r9);
    recv_from(sockfd, buf, len, flags, src_addr, addrlen)
}

fn recv_from(
    fd: i32,
    buf_addr: Addr,
    buf_len: usize,
    mut flags: i32,
    src_addr: Addr,
    src_len_addr: Addr,
) -> SysResult<usize> {
    if (buf_len as i32) < 0 {
        bail_libc!(libc::EINVAL);
    }
    let ctx = context::context();
    let file = {
        let mut task = ctx.task_mut();
        task.get_file(fd)
            .ok_or_else(|| SysError::new(libc::EBADF))?
    };
    let file = file.borrow();
    let socket = file
        .file_operations::<SocketFile>()
        .ok_or_else(|| SysError::new(libc::ENOTSOCK))?;
    if file.flags().non_blocking {
        flags |= libc::MSG_DONTWAIT;
    }
    let src_addr_and_len = if src_addr == Addr(0) {
        None
    } else {
        Some((src_addr, src_len_addr))
    };
    socket.recv_msg(buf_addr, buf_len as i32, flags, src_addr_and_len, &*ctx)
}

// getsockname implements linux syscall getsockopt(2)
pub fn getsockname(regs: &libc::user_regs_struct) -> super::Result {
    let sockfd = regs.rdi as i32;
    let addr = Addr(regs.rsi);
    let addr_len = Addr(regs.rdx);

    let ctx = context::context();
    let file = {
        let mut task = ctx.task_mut();
        task.get_file(sockfd)
            .ok_or_else(|| SysError::new(libc::EBADF))?
    };
    let file = file.borrow();
    let socket = file
        .file_operations::<SocketFile>()
        .ok_or_else(|| SysError::new(libc::ENOTSOCK))?;
    socket.get_sock_name(addr, addr_len, &*ctx).map(|()| 0)
}

// getpeername implements linux syscall getpeername(2)
pub fn getpeername(regs: &libc::user_regs_struct) -> super::Result {
    let sockfd = regs.rdi as i32;
    let addr = Addr(regs.rsi);
    let addr_len = Addr(regs.rdx);

    let ctx = context::context();
    let file = {
        let mut task = ctx.task_mut();
        task.get_file(sockfd)
            .ok_or_else(|| SysError::new(libc::EBADF))?
    };

    let file = file.borrow();
    let socket = file
        .file_operations::<SocketFile>()
        .ok_or_else(|| SysError::new(libc::ENOTSOCK))?;

    socket.get_peer_name(addr, addr_len, &*ctx).map(|()| 0)
}

const MAX_OPT_LEN: i32 = 1024 * 8;

// setsockopt implements linux syscall setsockopt(2)
pub fn setsockopt(regs: &libc::user_regs_struct) -> super::Result {
    let sockfd = regs.rdi as i32;
    let level = regs.rsi as i32;
    let optname = regs.rdx as i32;
    let optval_addr = Addr(regs.r10);
    let optlen = regs.r8 as i32;

    let ctx = context::context();
    let file = {
        let mut task = ctx.task_mut();
        task.get_file(sockfd)
            .ok_or_else(|| SysError::new(libc::EBADF))?
    };
    let file = file.borrow();
    let socket = file
        .file_operations::<SocketFile>()
        .ok_or_else(|| SysError::new(libc::ENOTSOCK))?;
    if !(0..=MAX_OPT_LEN).contains(&optlen) {
        bail_libc!(libc::EINVAL);
    }
    let task = ctx.task();
    let mut buf = vec![0; optlen as usize];
    task.copy_in_bytes(optval_addr, &mut buf)?;
    socket.set_sock_opt(level, optname, &buf, &*ctx).map(|()| 0)
}

// getsockopt implements linux syscall getsockopt(2)
pub fn getsockopt(regs: &libc::user_regs_struct) -> super::Result {
    let sockfd = regs.rdi as i32;
    let level = regs.rsi as i32;
    let optname = regs.rdx as i32;
    let optval_addr = Addr(regs.r10);
    let optlen_addr = Addr(regs.r8);

    let ctx = context::context();
    let file = {
        let mut task = ctx.task_mut();
        task.get_file(sockfd)
            .ok_or_else(|| SysError::new(libc::EBADF))?
    };
    let file = file.borrow();
    let socket = file
        .file_operations::<SocketFile>()
        .ok_or_else(|| SysError::new(libc::ENOTSOCK))?;
    let task = ctx.task();
    let optlen = {
        let mut optlen = [0; 4];
        task.copy_in_bytes(optlen_addr, &mut optlen)?;
        i32::from_be_bytes(optlen)
    };
    if optlen < 0 {
        bail_libc!(libc::EINVAL);
    }
    let v = socket.get_sock_opt(level, optname, optlen as u32, &*ctx)?;
    task.copy_out_bytes(optlen_addr, &v.len().to_le_bytes())?;
    task.copy_out_bytes(optval_addr, &v).map(|_| 0)
}

static MMSGHDR_SIZE: usize = std::mem::size_of::<libc::mmsghdr>();
static MSGHDR_SIZE: usize = std::mem::size_of::<libc::msghdr>();

// sendmmsg implements linux syscall sendmmsg(2)
pub fn sendmmsg(regs: &libc::user_regs_struct) -> super::Result {
    let sockfd = regs.rdi as i32;
    let msgvec_addr = Addr(regs.rsi);
    let vlen = regs.rdx as u32;
    let mut flags = regs.r10 as i32;

    let vlen = std::cmp::min(vlen, libc::UIO_MAXIOV as u32);
    let ctx = context::context();
    let file = {
        let mut task = ctx.task_mut();
        task.get_file(sockfd)
            .ok_or_else(|| SysError::new(libc::EBADF))?
    };
    let file = file.borrow();
    let socket = file
        .file_operations::<SocketFile>()
        .ok_or_else(|| SysError::new(libc::ENOTSOCK))?;

    if file.flags().non_blocking {
        flags |= libc::MSG_DONTWAIT;
    }

    let mut count = 0;
    for i in 0..vlen {
        let msghdr_addr = msgvec_addr
            .add_length((i as u64) * (MMSGHDR_SIZE as u64))
            .ok_or_else(|| SysError::new(libc::EFAULT))?;
        let msghdr = {
            let task = ctx.task();
            let mut dst = vec![0; MMSGHDR_SIZE];
            if let Err(err) = task.copy_in_bytes(msghdr_addr, &mut dst) {
                return if count == 0 { Err(err) } else { Ok(count) };
            }
            unsafe { *(dst.as_ptr() as *const libc::mmsghdr) }
        };
        let n = match send_single_msg(socket, msghdr.msg_hdr, flags) {
            Ok(n) => n,
            Err(err) => {
                return if count == 0 { Err(err) } else { Ok(count) };
            }
        };
        let msg_len = msghdr_addr
            .add_length(MSGHDR_SIZE as u64)
            .ok_or_else(|| SysError::new(libc::EFAULT))?;
        let task = ctx.task();
        if let Err(err) = task.copy_out_bytes(msg_len, &(n as u32).to_le_bytes()) {
            return if count == 0 { Err(err) } else { Ok(count) };
        }
        count += 1;
    }
    Ok(count)
}

fn send_single_msg(sock: &SocketFile, msg: libc::msghdr, flags: i32) -> SysResult<usize> {
    let ctx = context::context();
    let task = ctx.task();
    let mut src = task.iovecs_io_sequence(
        Addr(msg.msg_iov as u64),
        msg.msg_iovlen as i32,
        IoOpts {
            ignore_permissions: false,
        },
    )?;
    let to = match msg.msg_namelen {
        0 => None,
        len => {
            let mut buf = vec![0; len as usize];
            task.copy_in_bytes(Addr(msg.msg_name as u64), &mut buf)?;
            Some(buf)
        }
    };
    sock.send_msg(&mut src, to.as_deref(), flags, &*ctx)
}

// accept implements linux syscall accept(2)
pub fn accept(regs: &libc::user_regs_struct) -> super::Result {
    let sockfd = regs.rdi as i32;
    let sock_addr = Addr(regs.rsi);
    let len_addr = Addr(regs.rdx);
    accept_impl(sockfd, sock_addr, len_addr, 0)
}

// accept4 implements linux syscall accept4(2)
pub fn accept4(regs: &libc::user_regs_struct) -> super::Result {
    let sockfd = regs.rdi as i32;
    let sock_addr = Addr(regs.rsi);
    let len_addr = Addr(regs.rdx);
    let flags = regs.r10 as i32;
    accept_impl(sockfd, sock_addr, len_addr, flags)
}

fn accept_impl(sockfd: i32, sock_addr: Addr, len_addr: Addr, flags: i32) -> super::Result {
    if flags & !(libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC) != 0 {
        bail_libc!(libc::EINVAL);
    }
    let ctx = context::context();
    let (file, fd_flags) = {
        let mut task = ctx.task_mut();
        task.get_file_and_fd_flags(sockfd)
            .ok_or_else(|| SysError::new(libc::EBADF))?
    };
    let file = file.borrow();
    let socket = file
        .file_operations::<SocketFile>()
        .ok_or_else(|| SysError::new(libc::ENOTSOCK))?;
    let addr_and_len = if sock_addr.0 == 0 {
        None
    } else {
        Some((sock_addr, len_addr))
    };
    socket
        .accept(*file.flags(), fd_flags, addr_and_len, &*ctx)
        .map(|fd| fd as usize)
}
