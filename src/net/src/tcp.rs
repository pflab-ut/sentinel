use std::io::{Read, Write};

use mem::IoSequence;
use smoltcp::{iface::SocketHandle, socket::TcpSocket, wire::IpEndpoint};
use utils::{bail_libc, SysError, SysResult};

use crate::Context;

pub fn recv(
    handle: SocketHandle,
    dst: &mut IoSequence,
    peek: bool,
    non_blocking: bool,
    ctx: &dyn Context,
) -> SysResult<(usize, IpEndpoint)> {
    let start = std::time::Instant::now();
    let mut once = true;

    while {
        let mut iface = ctx.network_interface_mut();
        let socket = iface.get_socket::<TcpSocket>(handle);
        socket.may_recv() && !socket.can_recv()
    } {
        if non_blocking {
            bail_libc!(libc::EAGAIN);
        }
        ctx.poll_wait(once);
        once = false;
    }
    logger::debug!("tcp socket recv waited for {:?}", start.elapsed());

    let mut iface = ctx.network_interface_mut();
    let socket = iface.get_socket::<TcpSocket>(handle);
    let mut buf = vec![0; dst.num_bytes()];
    let endpoint = socket.remote_endpoint();
    let n = if peek {
        socket
            .peek_slice(&mut buf)
            .map_err(SysError::from_smoltcp_error)?
    } else {
        socket
            .recv_slice(&mut buf)
            .map_err(SysError::from_smoltcp_error)?
    };
    let n = dst.write(&buf[..n]).map_err(SysError::from_io_error)?;
    logger::debug!(
        "tcp socket recv elapsed: {:?} {} {}",
        start.elapsed(),
        socket.may_recv(),
        socket.can_recv()
    );
    Ok((n, endpoint))
}

pub fn send(
    handle: SocketHandle,
    src: &mut IoSequence,
    non_blocking: bool,
    ctx: &dyn Context,
) -> SysResult<usize> {
    let start = std::time::Instant::now();

    let mut once = true;
    while {
        let mut iface = ctx.network_interface_mut();
        let socket = iface.get_socket::<TcpSocket>(handle);
        socket.may_send() && !socket.can_send()
    } {
        if non_blocking {
            bail_libc!(libc::EAGAIN);
        }
        ctx.poll_wait(once);
        once = false;
    }
    logger::debug!("tcp socket recv waited for {:?}", start.elapsed());

    let mut iface = ctx.network_interface_mut();
    let socket = iface.get_socket::<TcpSocket>(handle);
    let mut buf = vec![0; src.num_bytes()];
    let n = src.read(&mut buf).map_err(SysError::from_io_error)?;
    let n = socket
        .send_slice(&buf[..n])
        .map_err(SysError::from_smoltcp_error)?;
    drop(iface);
    ctx.poll_wait(false);
    logger::debug!("tcp socket send elapsed: {:?}", start.elapsed());
    Ok(n)
}
