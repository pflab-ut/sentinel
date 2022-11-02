#![feature(unix_socket_ancillary_data)]

mod context;
mod tcp;
mod udp;
mod utils;

use std::{
    os::unix::{
        net::{UnixDatagram, UnixStream},
        prelude::{AsRawFd, FromRawFd, RawFd},
    },
    time::Duration,
};

pub use crate::utils::*;
use ::utils::{bail_libc, err_libc, SysError, SysResult};
pub use context::Context;
use mem::{Addr, IoSequence};
use smoltcp::{
    iface::SocketHandle,
    socket::{
        AnySocket, IcmpPacketMetadata, IcmpSocket, IcmpSocketBuffer, TcpSocket, TcpSocketBuffer,
        UdpPacketMetadata, UdpSocket, UdpSocketBuffer,
    },
    time::Duration as TDuration,
    wire::{IpAddress, IpEndpoint, Ipv4Address, Ipv6Address},
};

#[derive(Debug)]
pub enum Socket {
    UnixDatagram(Option<RawFd>),
    UnixStream(Option<RawFd>),
    Tcp {
        handle: SocketHandle,
        local_endpoint: IpEndpoint,
    },
    Udp {
        handle: SocketHandle,
        default_endpoint: Option<IpEndpoint>,
    },
    Icmp(SocketHandle),
}

impl Socket {
    pub fn new(domain: i32, stype: i32, protocol: i32, ctx: &dyn Context) -> SysResult<Self> {
        match domain {
            libc::AF_UNIX => {
                if protocol != 0 && protocol != libc::AF_UNIX {
                    bail_libc!(libc::EINVAL);
                }
                match stype {
                    libc::SOCK_DGRAM => {
                        let sock = UnixDatagram::unbound().map_err(SysError::from_io_error)?;
                        Ok(Self::UnixDatagram(Some(sock.as_raw_fd())))
                    }
                    libc::SOCK_STREAM => Ok(Self::UnixStream(None)),
                    _ => todo!("unhandled stype for Unix domain socket."),
                }
            }
            libc::AF_INET | libc::AF_INET6 => {
                match stype {
                    libc::SOCK_STREAM => {
                        if protocol != 0 && protocol != libc::IPPROTO_TCP {
                            bail_libc!(libc::EINVAL);
                        }
                        let rx_buffer = TcpSocketBuffer::new(vec![0; 65536]);
                        let tx_buffer = TcpSocketBuffer::new(vec![0; 65536]);
                        let socket = TcpSocket::new(rx_buffer, tx_buffer);
                        let handle = ctx.add_socket(socket.upcast());
                        Ok(Self::Tcp {
                            handle,
                            local_endpoint: IpEndpoint::UNSPECIFIED,
                        })
                    }
                    libc::SOCK_DGRAM => match protocol {
                        0 | libc::IPPROTO_UDP => {
                            let rx_buffer = UdpSocketBuffer::new(
                                vec![UdpPacketMetadata::EMPTY],
                                vec![0; 65536],
                            );
                            let tx_buffer = UdpSocketBuffer::new(
                                vec![UdpPacketMetadata::EMPTY],
                                vec![0; 65536],
                            );
                            let socket = UdpSocket::new(rx_buffer, tx_buffer);
                            let handle = ctx.add_socket(socket.upcast());
                            Ok(Self::Udp {
                                handle,
                                default_endpoint: None,
                            })
                        }
                        // FIXME: should handle this separately..?
                        libc::IPPROTO_ICMP | libc::IPPROTO_ICMPV6 => {
                            let rx_buffer = IcmpSocketBuffer::new(
                                vec![IcmpPacketMetadata::EMPTY],
                                vec![0; 65536],
                            );
                            let tx_buffer = IcmpSocketBuffer::new(
                                vec![IcmpPacketMetadata::EMPTY],
                                vec![0; 65536],
                            );
                            let socket = IcmpSocket::new(rx_buffer, tx_buffer);
                            let handle = ctx.add_socket(socket.upcast());
                            Ok(Self::Icmp(handle))
                        }
                        _ => {
                            logger::warn!(
                                "{}:{} procotol {} is not supported",
                                file!(),
                                line!(),
                                stype
                            );
                            bail_libc!(libc::EINVAL)
                        }
                    },
                    libc::SOCK_RAW => todo!("raw socket is not implemented yet"),
                    _ => {
                        logger::warn!(
                            "{}:{} procotol {} is not supported",
                            file!(),
                            line!(),
                            stype
                        );
                        bail_libc!(libc::EINVAL)
                    }
                }
            }
            _ => {
                logger::warn!("{}:{} unhandled domain {}", file!(), line!(), domain);
                bail_libc!(libc::EINVAL)
            }
        }
    }

    pub fn connect(&mut self, sock_addr: &[u8], domain: i32, ctx: &dyn Context) -> SysResult<()> {
        let (endpoint, dom) = address_and_family(sock_addr)?;
        if dom != domain as u16 {
            logger::warn!("specified domain does not match");
            bail_libc!(libc::EINVAL);
        }
        match (self, endpoint) {
            (&mut Self::UnixDatagram(fd), Endpoint::Unix(path)) => {
                let socket = unsafe { UnixDatagram::from_raw_fd(fd.unwrap()) };
                socket.connect(path).map_err(SysError::from_io_error)
            }
            (&mut Self::UnixStream(ref mut fd), Endpoint::Unix(path)) => {
                let socket = UnixStream::connect(path).map_err(SysError::from_io_error)?;
                *fd = Some(socket.as_raw_fd());
                Ok(())
            }
            (
                &mut Self::Tcp {
                    handle,
                    ref mut local_endpoint,
                },
                Endpoint::Ip(remote_endpoint),
            ) => {
                {
                    let mut iface = ctx.network_interface_mut();
                    let (socket, cx) = iface.get_socket_and_context::<TcpSocket>(handle);
                    // FIXME: what if blocking?
                    if !local_endpoint.is_specified() {
                        *local_endpoint = IpEndpoint::from(ctx.gen_local_port());
                    }
                    socket
                        .connect(cx, remote_endpoint, *local_endpoint)
                        .map_err(SysError::from_smoltcp_error)?;
                }
                loop {
                    ctx.poll_wait(false);
                    let mut iface = ctx.network_interface_mut();
                    let socket = iface.get_socket::<TcpSocket>(handle);
                    if socket.is_active() && socket.may_send() && socket.may_recv() {
                        break;
                    }
                }
                Ok(())
            }
            (
                &mut Self::Udp {
                    ref mut default_endpoint,
                    ..
                },
                Endpoint::Ip(ip_endpoint),
            ) => {
                *default_endpoint = Some(ip_endpoint);
                Ok(())
            }
            _ => {
                logger::warn!("endpoint type mismatch");
                bail_libc!(libc::EINVAL)
            }
        }
    }

    pub fn bind(&mut self, sock_addr: &[u8], domain: i32, ctx: &dyn Context) -> SysResult<()> {
        if sock_addr.len() < 2 {
            bail_libc!(libc::EINVAL);
        }
        let family = u16::from_le_bytes([sock_addr[0], sock_addr[1]]);
        if (family as i32) == libc::AF_PACKET {
            todo!()
        } else {
            let (endpoint, dom) = address_and_family(sock_addr)?;
            if dom != domain as u16 {
                logger::warn!("specified domain does not match");
                bail_libc!(libc::EINVAL);
            }
            match (self, endpoint) {
                (&mut Self::UnixDatagram(ref mut fd), Endpoint::Unix(path)) => {
                    let sock = UnixDatagram::bind(path).map_err(SysError::from_io_error)?;
                    *fd = Some(sock.as_raw_fd());
                    Ok(())
                }
                (&mut Self::UnixStream(ref mut fd), Endpoint::Unix(path)) => {
                    let sock = UnixStream::connect(path).map_err(SysError::from_io_error)?;
                    *fd = Some(sock.as_raw_fd());
                    Ok(())
                }
                (
                    &mut Self::Tcp {
                        ref mut local_endpoint,
                        ..
                    },
                    Endpoint::Ip(ip_endpoint),
                ) => {
                    *local_endpoint = ip_endpoint;
                    Ok(())
                }
                (&mut Self::Udp { handle, .. }, Endpoint::Ip(ip_endpoint)) => {
                    let mut iface = ctx.network_interface_mut();
                    let socket = iface.get_socket::<UdpSocket>(handle);
                    socket
                        .bind(ip_endpoint)
                        .map_err(SysError::from_smoltcp_error)
                }
                _ => {
                    logger::warn!("endpoint type mismatch");
                    bail_libc!(libc::EINVAL)
                }
            }
        }
    }

    pub fn readiness(&self, mask: u64, ctx: &dyn Context) -> u64 {
        match *self {
            Self::Tcp { handle, .. } => {
                let mut iface = ctx.network_interface_mut();
                let socket = iface.get_socket::<TcpSocket>(handle);
                let mut r = 0;
                if mask & linux::POLL_READABLE_EVENTS != 0 && socket.may_recv() {
                    r |= linux::POLL_READABLE_EVENTS;
                }
                if mask & linux::POLL_WRITABLE_EVENTS != 0 && socket.may_send() {
                    r |= linux::POLL_WRITABLE_EVENTS;
                }
                r
            }
            Self::Udp { handle, .. } => {
                let mut r = mask & linux::POLL_WRITABLE_EVENTS;
                if mask & linux::POLL_READABLE_EVENTS != 0 {
                    let mut iface = ctx.network_interface_mut();
                    let socket = iface.get_socket::<UdpSocket>(handle);
                    if socket.can_recv() {
                        r |= linux::POLL_READABLE_EVENTS;
                    }
                }
                r
            }
            Self::Icmp(handle) => {
                let mut r = mask & linux::POLL_WRITABLE_EVENTS;
                if mask & linux::POLL_READABLE_EVENTS != 0 {
                    let mut iface = ctx.network_interface_mut();
                    let socket = iface.get_socket::<IcmpSocket>(handle);
                    if socket.can_recv() {
                        r |= linux::POLL_READABLE_EVENTS;
                    }
                }
                r
            }
            Self::UnixStream(fd) => {
                get_poll_event_from_fd(fd.expect("FD for UnixStream is not set"), mask)
            }
            Self::UnixDatagram(fd) => {
                get_poll_event_from_fd(fd.expect("FD for UnixDatagram is not set"), mask)
            }
        }
    }

    pub fn send_msg(
        &self,
        src: &mut IoSequence,
        non_blocking: bool,
        addr_and_family: Option<(Endpoint<'_>, u16)>,
        ctx: &dyn Context,
    ) -> SysResult<usize> {
        match *self {
            Self::Tcp { handle, .. } => tcp::send(handle, src, non_blocking, ctx),
            Self::Udp {
                handle,
                default_endpoint,
            } => {
                let ep = match addr_and_family {
                    Some((ep, _family)) => match ep {
                        // TODO: Check family
                        Endpoint::Unix(_) => bail_libc!(libc::EINVAL),
                        Endpoint::Ip(ep) => ep,
                    },
                    None => default_endpoint.ok_or_else(|| SysError::new(libc::EINVAL))?,
                };
                udp::send(handle, src, non_blocking, ep, ctx)
            }
            Self::Icmp(_handle) => {
                todo!("send_msg for ICMP")
            }
            Self::UnixStream(_fd) => {
                todo!("send_msg for UnixStream")
            }
            Self::UnixDatagram(_fd) => {
                todo!("send_msg for UnixDatagram")
            }
        }
    }

    pub fn recv_msg(
        &self,
        dst: &mut IoSequence,
        peek: bool,
        non_blocking: bool,
        src_addr_and_len: Option<(Addr, Addr)>,
        ctx: &dyn Context,
    ) -> SysResult<usize> {
        let (n, endpoint) = match *self {
            Self::Tcp { handle, .. } => tcp::recv(handle, dst, peek, non_blocking, ctx)?,
            Self::Udp { handle, .. } => udp::recv(handle, dst, peek, non_blocking, ctx)?,
            _ => todo!("recv_msg"),
        };
        if let Some(s) = src_addr_and_len {
            self.write_socket_addr(endpoint, s, ctx)?;
        }
        Ok(n)
    }

    pub fn write_socket_addr(
        &self,
        endpoint: IpEndpoint,
        addr_and_len: (Addr, Addr),
        ctx: &dyn Context,
    ) -> SysResult<()> {
        let (addr, len) = addr_and_len;
        match endpoint.addr {
            IpAddress::Ipv4(ipv4) => {
                let s_addr = {
                    let bytes = ipv4.as_bytes();
                    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
                };
                let sockaddr = libc::sockaddr_in {
                    sin_family: libc::AF_INET as u16,
                    sin_port: endpoint.port.swap_bytes(),
                    sin_addr: libc::in_addr { s_addr },
                    sin_zero: [0; 8],
                };
                let src_bytes = unsafe {
                    std::slice::from_raw_parts(
                        &sockaddr as *const _ as *const u8,
                        std::mem::size_of::<libc::sockaddr_in>(),
                    )
                };
                let mut orig_src_len = [0; 4];
                ctx.copy_in_bytes(len, &mut orig_src_len)?;
                let orig_src_len = u32::from_le_bytes(orig_src_len) as usize;
                let src_bytes_len = src_bytes.len();
                ctx.copy_out_bytes(
                    addr,
                    &src_bytes[..std::cmp::min(src_bytes_len, orig_src_len)],
                )?;
                ctx.copy_out_bytes(len, &(src_bytes_len as u32).to_le_bytes())?;
                Ok(())
            }
            IpAddress::Ipv6(_) => todo!("writing out ipv6"),
            _ => {
                logger::warn!("remote address unspecified?: {:?}", endpoint.addr);
                bail_libc!(libc::EINVAL)
            }
        }
    }

    pub fn ioctl(&self, regs: &libc::user_regs_struct, ctx: &dyn Context) -> SysResult<usize> {
        match regs.rsi {
            libc::TIOCINQ => {
                let amount = self.recv_packet_size(ctx)?;
                let amount = std::cmp::min(amount, i32::MAX as usize) as i32;
                ctx.copy_out_bytes(Addr(regs.rdx), &amount.to_le_bytes())?;
                Ok(0)
            }
            _ => todo!(),
        }
    }

    fn recv_packet_size(&self, ctx: &dyn Context) -> SysResult<usize> {
        match *self {
            Self::Tcp { handle, .. } => {
                let mut iface = ctx.network_interface_mut();
                let socket = iface.get_socket::<TcpSocket>(handle);
                Ok(socket.recv_queue())
            }
            Self::Udp { handle, .. } => {
                let mut iface = ctx.network_interface_mut();
                let socket = iface.get_socket::<UdpSocket>(handle);
                match socket.peek() {
                    Ok((n, _)) => Ok(n.len()),
                    Err(err) => Err(SysError::from_smoltcp_error(err)),
                }
            }
            _ => todo!(),
        }
    }

    pub fn set_sock_opt_socket(
        &self,
        name: i32,
        optval: &[u8],
        ctx: &dyn Context,
    ) -> SysResult<()> {
        match *self {
            Self::Tcp { handle, .. } => {
                let mut iface = ctx.network_interface_mut();
                let socket = iface.get_socket::<TcpSocket>(handle);
                match name {
                    libc::SO_KEEPALIVE => {
                        if optval.len() < 4 {
                            bail_libc!(libc::EINVAL);
                        }
                        let v = u32::from_le_bytes([optval[0], optval[1], optval[2], optval[3]]);
                        let duration = if v == 0 {
                            None
                        } else {
                            Some(TDuration::from_secs(linux::DEFAULT_KEEPALIVE_SECS))
                        };
                        socket.set_keep_alive(duration);
                        Ok(())
                    }
                    _ => {
                        logger::warn!(
                            "Socket option {} is not yet implemented for Ip. Ignoring for now.",
                            name
                        );
                        Ok(())
                    }
                }
            }
            Self::Udp { .. } => {
                logger::warn!("Nothing to do for setsockopt on UDP socket for now..");
                Ok(())
            }
            Self::Icmp(_) => {
                logger::warn!("Nothing to do for setsockopt on ICMP socket for now..");
                Ok(())
            }
            Self::UnixStream(fd) => {
                let fd = fd.expect("File descriptor for UnixStream is not set.");
                let sock = unsafe { UnixStream::from_raw_fd(fd) };
                match name {
                    libc::SO_PASSCRED => {
                        if optval.len() < 4 {
                            bail_libc!(libc::EINVAL);
                        }
                        let v = u32::from_le_bytes([optval[0], optval[1], optval[2], optval[3]]);
                        sock.set_passcred(v != 0).map_err(SysError::from_io_error)
                    }
                    libc::SO_RCVTIMEO => {
                        if optval.len() < std::mem::size_of::<libc::timeval>() {
                            bail_libc!(libc::EINVAL);
                        }
                        let timeval = unsafe { *(optval.as_ptr() as *const libc::timeval) };
                        if timeval.tv_usec < 0 || timeval.tv_usec >= 1_000_000 {
                            bail_libc!(libc::EDOM);
                        }
                        let d_usec = Duration::from_micros(timeval.tv_usec as u64);
                        let d_sec = Duration::from_micros(timeval.tv_sec as u64);
                        sock.set_read_timeout(Some(d_usec + d_sec))
                            .map_err(SysError::from_io_error)
                    }
                    libc::SO_SNDTIMEO => {
                        if optval.len() < std::mem::size_of::<libc::timeval>() {
                            bail_libc!(libc::EINVAL);
                        }
                        let timeval = unsafe { *(optval.as_ptr() as *const libc::timeval) };
                        if timeval.tv_usec < 0 || timeval.tv_usec >= 1_000_000 {
                            bail_libc!(libc::EDOM);
                        }
                        let d_usec = Duration::from_micros(timeval.tv_usec as u64);
                        let d_sec = Duration::from_micros(timeval.tv_sec as u64);
                        sock.set_write_timeout(Some(d_usec + d_sec))
                            .map_err(SysError::from_io_error)
                    }
                    _ => {
                        logger::warn!(
                            "Socket option {} is not yet implemented for UnixStream. Ignoring for now.",
                            name
                        );
                        Ok(())
                    }
                }
            }
            Self::UnixDatagram(fd) => {
                let fd = fd.expect("File descriptor for UnixDatagram is not set.");
                let sock = unsafe { UnixDatagram::from_raw_fd(fd) };
                match name {
                    libc::SO_PASSCRED => {
                        if optval.len() < 4 {
                            bail_libc!(libc::EINVAL);
                        }
                        let v = u32::from_le_bytes([optval[0], optval[1], optval[2], optval[3]]);
                        sock.set_passcred(v != 0).map_err(SysError::from_io_error)
                    }
                    libc::SO_RCVTIMEO => {
                        if optval.len() < std::mem::size_of::<libc::timeval>() {
                            bail_libc!(libc::EINVAL);
                        }
                        let timeval = unsafe { *(optval.as_ptr() as *const libc::timeval) };
                        if timeval.tv_usec < 0 || timeval.tv_usec >= 1_000_000 {
                            bail_libc!(libc::EDOM);
                        }
                        let d_usec = Duration::from_micros(timeval.tv_usec as u64);
                        let d_sec = Duration::from_micros(timeval.tv_sec as u64);
                        sock.set_read_timeout(Some(d_usec + d_sec))
                            .map_err(SysError::from_io_error)
                    }
                    libc::SO_SNDTIMEO => {
                        if optval.len() < std::mem::size_of::<libc::timeval>() {
                            bail_libc!(libc::EINVAL);
                        }
                        let timeval = unsafe { *(optval.as_ptr() as *const libc::timeval) };
                        if timeval.tv_usec < 0 || timeval.tv_usec >= 1_000_000 {
                            bail_libc!(libc::EDOM);
                        }
                        let d_usec = Duration::from_micros(timeval.tv_usec as u64);
                        let d_sec = Duration::from_micros(timeval.tv_sec as u64);
                        sock.set_write_timeout(Some(d_usec + d_sec))
                            .map_err(SysError::from_io_error)
                    }
                    _ => {
                        logger::warn!(
                            "Socket option {} is not yet implemented for UnixDatagram. Ignoring for now.",
                            name
                        );
                        Ok(())
                    }
                }
            }
        }
    }

    pub fn set_sock_opt_tcp(&self, name: i32, optval: &[u8], ctx: &dyn Context) -> SysResult<()> {
        match *self {
            Self::Tcp { handle, .. } => {
                let mut iface = ctx.network_interface_mut();
                let socket = iface.get_socket::<TcpSocket>(handle);
                match name {
                    libc::TCP_NODELAY => {
                        if optval.len() < 4 {
                            bail_libc!(libc::EINVAL);
                        }
                        let v = u32::from_le_bytes([optval[0], optval[1], optval[2], optval[3]]);
                        socket.set_nagle_enabled(v == 0);
                        Ok(())
                    }
                    libc::TCP_QUICKACK => {
                        if optval.len() < 4 {
                            bail_libc!(libc::EINVAL);
                        }
                        let v = u32::from_le_bytes([optval[0], optval[1], optval[2], optval[3]]);
                        let d = if v != 0 {
                            None
                        } else {
                            Some(TDuration::from_millis(linux::DEFAULT_ACK_DELAY_MILLI_SECS))
                        };
                        socket.set_ack_delay(d);
                        Ok(())
                    }
                    libc::TCP_USER_TIMEOUT => {
                        if optval.len() < 4 {
                            bail_libc!(libc::EINVAL);
                        }
                        let v =
                            u32::from_le_bytes([optval[0], optval[1], optval[2], optval[3]]) as i32;
                        let d = if v < 0 {
                            bail_libc!(libc::EINVAL);
                        } else {
                            TDuration::from_millis(v as u64)
                        };
                        socket.set_timeout(Some(d));
                        Ok(())
                    }
                    _ => {
                        logger::warn!(
                            "Socket option {} is not yet implemented for TCP socket. Ignoring for now.",
                            name
                        );
                        Ok(())
                    }
                }
            }
            _ => {
                logger::warn!("SOL_TCP is only supported for TCP sockets.");
                bail_libc!(libc::ENOPROTOOPT)
            }
        }
    }

    pub fn set_sock_opt_ip(&self, name: i32, optval: &[u8], ctx: &dyn Context) -> SysResult<()> {
        match *self {
            Self::Tcp { handle, .. } => {
                let mut iface = ctx.network_interface_mut();
                let socket = iface.get_socket::<TcpSocket>(handle);
                match name {
                    libc::IP_MULTICAST_TTL => {
                        if optval.len() < 4 {
                            bail_libc!(libc::EINVAL);
                        }
                        let v =
                            u32::from_le_bytes([optval[0], optval[1], optval[2], optval[3]]) as i32;
                        if v < -1 || 255 < v {
                            bail_libc!(libc::EINVAL);
                        }
                        let lim = if v == -1 {
                            linux::IP_DEFAULT_MCAST_TTL
                        } else {
                            v as u8
                        };
                        socket.set_hop_limit(Some(lim));
                        Ok(())
                    }
                    _ => {
                        logger::warn!(
                            "Socket option {} is not yet implemented for TCP socket. Ignoring for now.",
                            name
                        );
                        Ok(())
                    }
                }
            }
            Self::Udp { handle, .. } => {
                let mut iface = ctx.network_interface_mut();
                let socket = iface.get_socket::<UdpSocket>(handle);
                match name {
                    libc::IP_MULTICAST_TTL => {
                        if optval.len() < 4 {
                            bail_libc!(libc::EINVAL);
                        }
                        let v =
                            u32::from_le_bytes([optval[0], optval[1], optval[2], optval[3]]) as i32;
                        if v < -1 || 255 < v {
                            bail_libc!(libc::EINVAL);
                        }
                        let lim = if v == -1 {
                            linux::IP_DEFAULT_MCAST_TTL
                        } else {
                            v as u8
                        };
                        socket.set_hop_limit(Some(lim));
                        Ok(())
                    }
                    _ => {
                        logger::warn!(
                            "Socket option {} (IPV4) is not yet implemented for UDP socket. Ignoring for now.",
                            name
                        );
                        Ok(())
                    }
                }
            }
            _ => {
                logger::warn!("SOL_IP is only supported for TCP and UDP sockets.");
                bail_libc!(libc::ENOPROTOOPT)
            }
        }
    }

    pub fn set_sock_opt_ipv6(&self, name: i32, optval: &[u8], ctx: &dyn Context) -> SysResult<()> {
        match *self {
            Self::Tcp { handle, .. } => {
                let mut iface = ctx.network_interface_mut();
                let socket = iface.get_socket::<TcpSocket>(handle);
                match name {
                    libc::IPV6_MULTICAST_HOPS => {
                        if optval.len() < 4 {
                            bail_libc!(libc::EINVAL);
                        }
                        let v =
                            u32::from_le_bytes([optval[0], optval[1], optval[2], optval[3]]) as i32;
                        if v < -1 || 255 < v {
                            bail_libc!(libc::EINVAL);
                        }
                        let lim = if v == -1 {
                            linux::IPV6_DEFAULT_MCAST_HOPS
                        } else {
                            v as u8
                        };
                        socket.set_hop_limit(Some(lim));
                        Ok(())
                    }
                    _ => {
                        logger::warn!(
                            "Socket option {} is not yet implemented for TCP socket. Ignoring for now.",
                            name
                        );
                        Ok(())
                    }
                }
            }
            Self::Udp { handle, .. } => {
                let mut iface = ctx.network_interface_mut();
                let socket = iface.get_socket::<UdpSocket>(handle);
                match name {
                    libc::IPV6_MULTICAST_HOPS => {
                        if optval.len() < 4 {
                            bail_libc!(libc::EINVAL);
                        }
                        let v =
                            u32::from_le_bytes([optval[0], optval[1], optval[2], optval[3]]) as i32;
                        if v < -1 || 255 < v {
                            bail_libc!(libc::EINVAL);
                        }
                        let lim = if v == -1 {
                            linux::IPV6_DEFAULT_MCAST_HOPS
                        } else {
                            v as u8
                        };
                        socket.set_hop_limit(Some(lim));
                        Ok(())
                    }
                    _ => {
                        logger::warn!(
                            "Socket option {} (IPV6) is not yet implemented for UDP socket. Ignoring for now.",
                            name
                        );
                        Ok(())
                    }
                }
            }
            _ => {
                logger::warn!("SOL_IPV6 is only supported for TCP and UDP sockets.");
                bail_libc!(libc::ENOPROTOOPT)
            }
        }
    }

    pub fn get_sock_opt_socket(
        &self,
        name: i32,
        optlen: u32,
        ctx: &dyn Context,
    ) -> SysResult<Vec<u8>> {
        match *self {
            Self::Tcp { handle, .. } => {
                let mut iface = ctx.network_interface_mut();
                let socket = iface.get_socket::<TcpSocket>(handle);
                match name {
                    libc::SO_KEEPALIVE => {
                        if optlen < 4 {
                            bail_libc!(libc::EINVAL);
                        }
                        let v: i32 = if socket.keep_alive().is_some() { 1 } else { 0 };
                        Ok(v.to_le_bytes().to_vec())
                    }
                    libc::SO_SNDBUF => {
                        if optlen < 4 {
                            bail_libc!(libc::EINVAL);
                        }
                        let size = std::cmp::min(socket.send_capacity(), i32::MAX as usize) as i32;
                        Ok(size.to_le_bytes().to_vec())
                    }
                    libc::SO_RCVBUF => {
                        if optlen < 4 {
                            bail_libc!(libc::EINVAL);
                        }
                        let size = std::cmp::min(socket.recv_capacity(), i32::MAX as usize) as i32;
                        Ok(size.to_le_bytes().to_vec())
                    }
                    _ => {
                        logger::warn!(
                            "Socket option {} is not yet implemented for TCP socket. Ignoring for now.",
                            name
                        );
                        Ok(vec![0; 4])
                    }
                }
            }
            Self::Udp { handle, .. } => {
                let mut iface = ctx.network_interface_mut();
                let socket = iface.get_socket::<UdpSocket>(handle);
                match name {
                    libc::SO_SNDBUF => {
                        if optlen < 4 {
                            bail_libc!(libc::EINVAL);
                        }
                        let size =
                            std::cmp::min(socket.payload_send_capacity(), i32::MAX as usize) as i32;
                        Ok(size.to_le_bytes().to_vec())
                    }
                    libc::SO_RCVBUF => {
                        if optlen < 4 {
                            bail_libc!(libc::EINVAL);
                        }
                        let size =
                            std::cmp::min(socket.payload_recv_capacity(), i32::MAX as usize) as i32;
                        Ok(size.to_le_bytes().to_vec())
                    }
                    _ => {
                        logger::warn!(
                            "Socket option {} is not yet implemented for TCP socket. Ignoring for now.",
                            name
                        );
                        Ok(vec![0; 4])
                    }
                }
            }
            _ => todo!("get_sock_opt_socket"),
        }
    }

    pub fn get_sock_opt_tcp(
        &self,
        name: i32,
        optlen: u32,
        ctx: &dyn Context,
    ) -> SysResult<Vec<u8>> {
        match *self {
            Self::Tcp { handle, .. } => {
                let mut iface = ctx.network_interface_mut();
                let socket = iface.get_socket::<TcpSocket>(handle);
                match name {
                    libc::TCP_NODELAY => {
                        if optlen < 4 {
                            bail_libc!(libc::EINVAL);
                        }
                        let v: i32 = if socket.nagle_enabled().is_none() {
                            1
                        } else {
                            0
                        };
                        Ok(v.to_le_bytes().to_vec())
                    }
                    libc::TCP_QUICKACK => {
                        if optlen < 4 {
                            bail_libc!(libc::EINVAL);
                        }
                        let v = socket
                            .ack_delay()
                            .map(|d| d == TDuration::ZERO)
                            .unwrap_or(true);
                        let v: i32 = if v { 1 } else { 0 };
                        Ok(v.to_le_bytes().to_vec())
                    }
                    libc::TCP_USER_TIMEOUT => {
                        if optlen < 4 {
                            bail_libc!(libc::EINVAL);
                        }
                        let d = socket.timeout().unwrap_or(TDuration::ZERO);
                        let v = d.millis() as i32;
                        Ok(v.to_le_bytes().to_vec())
                    }
                    _ => {
                        logger::warn!(
                            "Socket option {} is not yet implemented for TCP socket. Ignoring for now.",
                            name
                        );
                        Ok(vec![0; 4])
                    }
                }
            }
            _ => {
                logger::warn!("SOL_TCP is only supported for TCP sockets.");
                bail_libc!(libc::ENOPROTOOPT)
            }
        }
    }

    pub fn get_sock_opt_ip(&self, name: i32, optlen: u32, ctx: &dyn Context) -> SysResult<Vec<u8>> {
        match *self {
            Self::Tcp { handle, .. } => {
                let mut iface = ctx.network_interface_mut();
                let socket = iface.get_socket::<TcpSocket>(handle);
                match name {
                    libc::IP_MULTICAST_TTL => {
                        if optlen < 4 {
                            bail_libc!(libc::EINVAL);
                        }
                        Ok(socket.hop_limit().unwrap_or(0).to_le_bytes().to_vec())
                    }
                    _ => {
                        logger::warn!(
                            "Socket option {} is not yet implemented for TCP socket. Ignoring for now.",
                            name
                        );
                        Ok(vec![0; 4])
                    }
                }
            }
            Self::Udp { handle, .. } => {
                let mut iface = ctx.network_interface_mut();
                let socket = iface.get_socket::<UdpSocket>(handle);
                match name {
                    libc::IP_MULTICAST_TTL => {
                        if optlen < 4 {
                            bail_libc!(libc::EINVAL);
                        }
                        Ok(socket.hop_limit().unwrap_or(0).to_le_bytes().to_vec())
                    }
                    _ => {
                        logger::warn!(
                            "Socket option {} (IPV4) is not yet implemented for UDP socket. Ignoring for now.",
                            name
                        );
                        Ok(vec![0; 4])
                    }
                }
            }
            _ => {
                logger::warn!("SOL_IP is only supported for TCP and UDP sockets.");
                bail_libc!(libc::ENOPROTOOPT)
            }
        }
    }

    pub fn get_sock_opt_ipv6(
        &self,
        name: i32,
        optlen: u32,
        ctx: &dyn Context,
    ) -> SysResult<Vec<u8>> {
        match *self {
            Self::Tcp { handle, .. } => {
                let mut iface = ctx.network_interface_mut();
                let socket = iface.get_socket::<TcpSocket>(handle);
                match name {
                    libc::IPV6_MULTICAST_HOPS => {
                        if optlen < 4 {
                            bail_libc!(libc::EINVAL);
                        }
                        Ok(socket.hop_limit().unwrap_or(0).to_le_bytes().to_vec())
                    }
                    _ => {
                        logger::warn!(
                            "Socket option {} is not yet implemented for TCP socket. Ignoring for now.",
                            name
                        );
                        Ok(vec![0; 4])
                    }
                }
            }
            Self::Udp { handle, .. } => {
                let mut iface = ctx.network_interface_mut();
                let socket = iface.get_socket::<UdpSocket>(handle);
                match name {
                    libc::IPV6_MULTICAST_HOPS => {
                        if optlen < 4 {
                            bail_libc!(libc::EINVAL);
                        }
                        Ok(socket.hop_limit().unwrap_or(0).to_le_bytes().to_vec())
                    }
                    _ => {
                        logger::warn!(
                            "Socket option {} (IPV6) is not yet implemented for UDP socket. Ignoring for now.",
                            name
                        );
                        Ok(vec![0; 4])
                    }
                }
            }
            _ => {
                logger::warn!("SOL_IPV6 is only supported for TCP and UDP sockets.");
                bail_libc!(libc::ENOPROTOOPT)
            }
        }
    }

    pub fn local_endpoint(&self, ctx: &dyn Context) -> IpEndpoint {
        match *self {
            Self::Tcp { local_endpoint, .. } => local_endpoint,
            Self::Udp { handle, .. } => {
                let mut iface = ctx.network_interface_mut();
                let socket = iface.get_socket::<UdpSocket>(handle);
                socket.endpoint()
            }
            _ => todo!("local_endpoint"),
        }
    }

    pub fn remote_endpoint(&self, ctx: &dyn Context) -> Option<IpEndpoint> {
        match *self {
            Self::Tcp { handle, .. } => {
                let mut iface = ctx.network_interface_mut();
                let socket = iface.get_socket::<TcpSocket>(handle);
                Some(socket.remote_endpoint())
            }
            Self::Udp {
                default_endpoint, ..
            } => default_endpoint,
            _ => todo!("local_endpoint"),
        }
    }

    pub fn write(
        &self,
        src: &mut IoSequence,
        non_blocking: bool,
        ctx: &dyn Context,
    ) -> SysResult<usize> {
        match *self {
            Self::Tcp { handle, .. } => tcp::send(handle, src, non_blocking, ctx),
            Self::Udp {
                handle,
                default_endpoint,
            } => {
                let mut iface = ctx.network_interface_mut();
                let socket = iface.get_socket::<UdpSocket>(handle);
                let endpoint = if socket.endpoint().is_specified() {
                    socket.endpoint()
                } else {
                    default_endpoint.ok_or_else(|| SysError::new(libc::EINVAL))?
                };
                udp::send(handle, src, non_blocking, endpoint, ctx)
            }
            _ => todo!("write to socket"),
        }
    }

    pub fn listen(&mut self, ctx: &dyn Context) -> SysResult<()> {
        match self {
            &mut Self::Tcp {
                handle,
                ref mut local_endpoint,
            } => {
                if !local_endpoint.is_specified() {
                    *local_endpoint = IpEndpoint::from(ctx.gen_local_port());
                }
                let mut iface = ctx.network_interface_mut();
                let socket = iface.get_socket::<TcpSocket>(handle);
                socket
                    .listen(local_endpoint.port)
                    .map_err(SysError::from_smoltcp_error)
            }
            _ => err_libc!(libc::EOPNOTSUPP),
        }
    }
}

// impl Drop for Socket {
//     fn drop(&mut self) {
//         match *self {
//             Self::Tcp { handle, .. } => {
//                 let ctx = crate::context::context();
//                 let mut iface = ctx.network_interface_mut();
//                 let socket = iface.get_socket::<TcpSocket>(handle);
//                 let port = socket.remote_endpoint().port;
//                 ctx.remove_local_port(port);
//                 socket.abort();
//                 iface.remove_socket(handle);
//             }
//             Self::Udp { handle, .. } => {
//                 let ctx = crate::context::context();
//                 let mut iface = ctx.network_interface_mut();
//                 let socket = iface.get_socket::<UdpSocket>(handle);
//                 let port = socket.endpoint().port;
//                 ctx.remove_local_port(port);
//                 socket.close();
//                 iface.remove_socket(handle);
//             }
//             Self::Icmp(handle) => {
//                 let ctx = crate::context::context();
//                 let mut iface = ctx.network_interface_mut();
//                 iface.remove_socket(handle);
//             }
//             Self::UnixDatagram(fd) => {
//                 if let Some(fd) = fd {
//                     let socket = unsafe { UnixDatagram::from_raw_fd(fd) };
//                     socket.shutdown(Shutdown::Both).unwrap();
//                 }
//             }
//             Self::UnixStream(fd) => {
//                 if let Some(fd) = fd {
//                     let socket = unsafe { UnixStream::from_raw_fd(fd) };
//                     socket.shutdown(Shutdown::Both).unwrap();
//                 }
//             }
//         }
//     }
// }

#[derive(Debug)]
pub enum Endpoint<'a> {
    Unix(&'a str),
    Ip(IpEndpoint),
}

pub fn address_and_family(addr: &[u8]) -> SysResult<(Endpoint<'_>, u16)> {
    if addr.len() < 2 {
        bail_libc!(libc::EINVAL);
    }
    let dom = u16::from_le_bytes([addr[0], addr[1]]);
    match dom as i32 {
        libc::AF_INET => {
            let sock_addr = unsafe { std::ptr::read(addr.as_ptr() as *const libc::sockaddr_in) };
            let ipv4 = Ipv4Address::from_bytes(&sock_addr.sin_addr.s_addr.to_le_bytes());
            let ip_endpoint = IpEndpoint {
                addr: IpAddress::Ipv4(ipv4),
                port: sock_addr.sin_port.swap_bytes(),
            };
            Ok((Endpoint::Ip(ip_endpoint), dom))
        }
        libc::AF_INET6 => {
            let sock_addr = unsafe { std::ptr::read(addr.as_ptr() as *const libc::sockaddr_in6) };
            let ipv6 = Ipv6Address::from_bytes(&sock_addr.sin6_addr.s6_addr);
            let ip_endpoint = IpEndpoint {
                addr: IpAddress::Ipv6(ipv6),
                port: sock_addr.sin6_port.swap_bytes(),
            };
            Ok((Endpoint::Ip(ip_endpoint), dom))
        }
        libc::AF_UNIX => {
            let addr = truncate_path(&addr[2..]);
            let sock_addr = std::str::from_utf8(addr)
                .map_err(|_| SysError::new_with_msg(libc::EINVAL, "utf8 error".to_string()))?;
            Ok((Endpoint::Unix(sock_addr), dom))
        }
        libc::AF_PACKET => todo!("packet socket is yet to be supported."),
        _ => {
            logger::warn!("unsupported family");
            bail_libc!(libc::EINVAL)
        }
    }
}

fn truncate_path(path: &[u8]) -> &[u8] {
    for (i, c) in path.iter().enumerate() {
        if *c == 0 {
            return &path[..i];
        }
    }
    path
}
