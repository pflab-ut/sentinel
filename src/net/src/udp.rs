use std::io::{Read, Write};

use mem::IoSequence;
use smoltcp::{iface::SocketHandle, socket::UdpSocket, wire::IpEndpoint};
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
        let socket = iface.get_socket::<UdpSocket>(handle);
        !socket.can_recv()
    } {
        if non_blocking {
            bail_libc!(libc::EAGAIN);
        }
        ctx.poll_wait(once);
        once = false;
    }
    logger::debug!("udp socket recv waited for {:?}", start.elapsed());

    let mut iface = ctx.network_interface_mut();
    let socket = iface.get_socket::<UdpSocket>(handle);
    let (buf, endpoint) = if peek {
        socket
            .peek()
            .map_err(SysError::from_smoltcp_error)
            .map(|(s, e)| (s, *e))?
    } else {
        socket.recv().map_err(SysError::from_smoltcp_error)?
    };
    let n = dst.write(buf).map_err(SysError::from_io_error)?;
    logger::debug!("udp socket recv elapsed: {:?}", start.elapsed());
    Ok((n, endpoint))
}

pub fn send(
    handle: SocketHandle,
    src: &mut IoSequence,
    non_blocking: bool,
    endpoint: IpEndpoint,
    ctx: &dyn Context,
) -> SysResult<usize> {
    let start = std::time::Instant::now();

    let mut once = true;
    while {
        let mut iface = ctx.network_interface_mut();
        let socket = iface.get_socket::<UdpSocket>(handle);
        !socket.can_send()
    } {
        if non_blocking {
            bail_libc!(libc::EAGAIN);
        }
        ctx.poll_wait(once);
        once = false;
    }

    let mut iface = ctx.network_interface_mut();
    let socket = iface.get_socket::<UdpSocket>(handle);
    if socket.endpoint().port == 0 {
        let port = ctx.gen_local_port();
        socket.bind(port).map_err(SysError::from_smoltcp_error)?;
    }
    let mut buf = vec![0; src.num_bytes()];
    let n = src.read(&mut buf).map_err(SysError::from_io_error)?;
    loop {
        match socket.send_slice(&buf[..n], endpoint) {
            Ok(()) => {
                logger::debug!("udp socket send elapsed: {:?}", start.elapsed(),);
                drop(iface);
                ctx.poll_wait(false);
                return Ok(n);
            }
            Err(err) if err == smoltcp::Error::Exhausted => {
                if non_blocking {
                    bail_libc!(libc::EAGAIN);
                }
            }
            Err(err) => return Err(SysError::from_smoltcp_error(err)),
        };
    }
}
