use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, Mutex},
};

use mem::{Addr, IoOpts, IoSequence, PAGE_SIZE};
use memmap::mmap_opts::MmapOpts;
use once_cell::sync::Lazy;

use dev::Device;
use net::{address_and_family, Socket};
use smoltcp::{socket::TcpSocket, wire::IpEndpoint};
use utils::{bail_libc, err_libc, SysError, SysResult};

use crate::{
    attr::{FilePermissions, InodeType, PermMask, StableAttr},
    dentry::DentrySerializer,
    fsutils::inode::{InodeSimpleAttributes, SimpleFileInode},
    inode::Inode,
    mount::MountSource,
    seek::SeekWhence,
    Context, Dirent, DirentRef, FdFlags, File, FileFlags, FileOperations, ReaddirError,
    ReaddirResult,
};

pub static NET_DEVICE: Lazy<Arc<Mutex<Device>>> = Lazy::new(Device::new_anonymous_device);

#[derive(Debug)]
pub struct SocketFile {
    socket: Socket,
    pub domain: i32,
    pub stype: i32,
    pub protocol: i32,
    dirent: DirentRef,
    sockopt_timestamp: Mutex<bool>,
    sockopt_inq: Mutex<bool>,
}

impl FileOperations for SocketFile {
    fn dirent(&self) -> DirentRef {
        self.dirent.clone()
    }

    fn read(
        &self,
        flags: FileFlags,
        dst: &mut IoSequence,
        _: i64,
        ctx: &dyn Context,
    ) -> SysResult<usize> {
        if dst.num_bytes() == 0 {
            return Ok(0);
        }
        self.socket
            .recv_msg(dst, false, flags.non_blocking, None, ctx.as_net_context())
    }

    fn write(
        &self,
        flags: FileFlags,
        src: &mut IoSequence,
        _: i64,
        ctx: &dyn Context,
    ) -> SysResult<usize> {
        self.socket
            .write(src, flags.non_blocking, ctx.as_net_context())
    }

    fn configure_mmap(&mut self, _: &mut MmapOpts) -> SysResult<()> {
        bail_libc!(libc::ENODEV)
    }

    fn flush(&self) -> SysResult<()> {
        Ok(())
    }

    fn close(&self) -> SysResult<()> {
        Ok(())
    }

    fn ioctl(&self, regs: &libc::user_regs_struct, ctx: &dyn Context) -> SysResult<usize> {
        self.socket.ioctl(regs, ctx.as_net_context())
    }

    fn seek(&mut self, _: &Inode, _: SeekWhence, _: i64, _: i64) -> SysResult<i64> {
        bail_libc!(libc::ESPIPE)
    }
    fn readdir(
        &mut self,
        _: i64,
        _: &mut dyn DentrySerializer,
        _: &dyn Context,
    ) -> ReaddirResult<i64> {
        Err(ReaddirError::new(0, libc::ENOTDIR))
    }
    fn readiness(&self, mask: u64, ctx: &dyn Context) -> u64 {
        self.socket.readiness(mask, ctx.as_net_context())
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl SocketFile {
    pub fn new(
        domain: i32,
        stype: i32,
        protocol: i32,
        dirent: DirentRef,
        ctx: &dyn Context,
    ) -> SysResult<Self> {
        let socket = Socket::new(domain, stype, protocol, ctx.as_net_context())?;
        Ok(Self {
            socket,
            domain,
            stype,
            protocol,
            dirent,
            sockopt_timestamp: Mutex::new(false),
            sockopt_inq: Mutex::new(false),
        })
    }

    pub fn connect(
        &mut self,
        sock_addr: &[u8],
        _blocking: bool,
        ctx: &dyn Context,
    ) -> SysResult<()> {
        self.socket
            .connect(sock_addr, self.domain, ctx.as_net_context())
    }

    pub fn bind(&mut self, sock_addr: &[u8], ctx: &dyn Context) -> SysResult<()> {
        self.socket
            .bind(sock_addr, self.domain, ctx.as_net_context())
    }

    pub fn set_sock_opt(
        &self,
        level: i32,
        name: i32,
        optval: &[u8],
        ctx: &dyn Context,
    ) -> SysResult<()> {
        match level {
            libc::SOL_SOCKET if name == libc::SO_TIMESTAMP => {
                if optval.len() < 4 {
                    bail_libc!(libc::EINVAL);
                }
                let mut so_timestamp = self.sockopt_timestamp.lock().unwrap();
                *so_timestamp =
                    u32::from_le_bytes([optval[0], optval[1], optval[2], optval[3]]) != 0;
                Ok(())
            }
            libc::SOL_SOCKET => self
                .socket
                .set_sock_opt_socket(name, optval, ctx.as_net_context()),
            libc::SOL_TCP if name == libc::TCP_INQ => {
                if optval.len() < 4 {
                    bail_libc!(libc::EINVAL);
                }
                let mut so_timestamp = self.sockopt_inq.lock().unwrap();
                *so_timestamp =
                    u32::from_le_bytes([optval[0], optval[1], optval[2], optval[3]]) != 0;
                Ok(())
            }
            libc::SOL_TCP => self
                .socket
                .set_sock_opt_tcp(name, optval, ctx.as_net_context()),
            libc::SOL_IP => self
                .socket
                .set_sock_opt_ip(name, optval, ctx.as_net_context()),
            libc::SOL_IPV6 => self
                .socket
                .set_sock_opt_ipv6(name, optval, ctx.as_net_context()),
            _ => {
                logger::warn!("Unsupported setsockopt level: {}", level);
                Ok(())
            }
        }
    }

    pub fn get_sock_opt(
        &self,
        level: i32,
        name: i32,
        optval_len: u32,
        ctx: &dyn Context,
    ) -> SysResult<Vec<u8>> {
        match level {
            libc::SOL_SOCKET if name == libc::SO_TYPE => {
                if optval_len < 4 {
                    bail_libc!(libc::EINVAL);
                }
                Ok(self.stype.to_le_bytes().to_vec())
            }
            libc::SOL_SOCKET if name == libc::SO_DOMAIN => {
                if optval_len < 4 {
                    bail_libc!(libc::EINVAL);
                }
                Ok(self.domain.to_le_bytes().to_vec())
            }
            libc::SOL_SOCKET if name == libc::SO_PROTOCOL => {
                if optval_len < 4 {
                    bail_libc!(libc::EINVAL);
                }
                Ok(self.protocol.to_le_bytes().to_vec())
            }
            libc::SOL_SOCKET if name == libc::SO_TIMESTAMP => {
                if optval_len < 4 {
                    bail_libc!(libc::EINVAL);
                }
                let val = if *self.sockopt_timestamp.lock().unwrap() {
                    1i32
                } else {
                    0i32
                };
                Ok(val.to_le_bytes().to_vec())
            }
            libc::SOL_SOCKET => {
                self.socket
                    .get_sock_opt_socket(name, optval_len, ctx.as_net_context())
            }
            libc::SOL_TCP if name == libc::TCP_INQ => {
                if optval_len < 4 {
                    bail_libc!(libc::EINVAL);
                }
                let val = if *self.sockopt_inq.lock().unwrap() {
                    1i32
                } else {
                    0i32
                };
                Ok(val.to_le_bytes().to_vec())
            }
            libc::SOL_TCP => self
                .socket
                .get_sock_opt_tcp(name, optval_len, ctx.as_net_context()),
            libc::SOL_IP => self
                .socket
                .get_sock_opt_ip(name, optval_len, ctx.as_net_context()),
            libc::SOL_IPV6 => self
                .socket
                .get_sock_opt_ipv6(name, optval_len, ctx.as_net_context()),
            _ => {
                logger::warn!("Unsupported getsockopt level: {}", level);
                Ok(vec![0; 4])
            }
        }
    }

    pub fn get_sock_name(
        &self,
        sock_addr: Addr,
        sock_addr_len: Addr,
        ctx: &dyn Context,
    ) -> SysResult<()> {
        let endpoint = self.socket.local_endpoint(ctx.as_net_context());
        self.socket
            .write_socket_addr(endpoint, (sock_addr, sock_addr_len), ctx.as_net_context())
    }

    pub fn get_peer_name(
        &self,
        sock_addr: Addr,
        sock_addr_len: Addr,
        ctx: &dyn Context,
    ) -> SysResult<()> {
        match self.socket.remote_endpoint(ctx.as_net_context()) {
            Some(endpoint) => self.socket.write_socket_addr(
                endpoint,
                (sock_addr, sock_addr_len),
                ctx.as_net_context(),
            ),
            None => Ok(()),
        }
    }

    pub fn send_msg(
        &self,
        src: &mut IoSequence,
        to: Option<&[u8]>,
        flags: i32,
        ctx: &dyn Context,
    ) -> SysResult<usize> {
        let addr_and_family = to.map(address_and_family).transpose()?;
        self.socket.send_msg(
            src,
            flags & libc::MSG_DONTWAIT != 0,
            addr_and_family,
            ctx.as_net_context(),
        )
    }

    pub fn recv_msg(
        &self,
        buf: Addr,
        len: i32,
        flags: i32,
        src_addr_and_len: Option<(Addr, Addr)>,
        ctx: &dyn Context,
    ) -> SysResult<usize> {
        if flags & libc::MSG_ERRQUEUE != 0 {
            todo!()
        }
        let mut dst = ctx.single_io_sequence(
            buf,
            len,
            IoOpts {
                ignore_permissions: false,
            },
        )?;
        // TODO: More flag handling.
        self.socket.recv_msg(
            &mut dst,
            flags & libc::MSG_PEEK != 0,
            flags & libc::MSG_DONTWAIT != 0,
            src_addr_and_len,
            ctx.as_net_context(),
        )
    }

    // FIXME: Make proper use of `backlog`.
    pub fn listen(&mut self, _backlog: i32, ctx: &dyn Context) -> SysResult<()> {
        match &mut self.socket {
            &mut Socket::Tcp {
                ref mut local_endpoint,
                ..
            } => {
                if !local_endpoint.is_specified() {
                    *local_endpoint = IpEndpoint::from(ctx.gen_local_port());
                }
                Ok(())
            }
            _ => err_libc!(libc::EOPNOTSUPP),
        }
    }

    pub fn accept(
        &self,
        file_flags: FileFlags,
        fd_flags: FdFlags,
        addr_and_len: Option<(Addr, Addr)>,
        ctx: &dyn Context,
    ) -> SysResult<i32> {
        let mut file = build_socket_file(self.domain, self.stype, self.protocol, ctx)?;
        let socket_file = file.file_operations_mut::<SocketFile>().unwrap();
        let handle = match socket_file.socket {
            Socket::Tcp {
                handle,
                ref mut local_endpoint,
            } => {
                *local_endpoint = self.socket.local_endpoint(ctx.as_net_context());
                handle
            }
            _ => bail_libc!(libc::EOPNOTSUPP),
        };

        socket_file.socket.listen(ctx.as_net_context())?;
        loop {
            ctx.poll_wait(false);
            let mut iface = ctx.network_interface_mut();
            let socket = iface.get_socket::<TcpSocket>(handle);
            if socket.is_active() && socket.may_recv() {
                if let Some(addr_and_len) = addr_and_len {
                    self.socket.write_socket_addr(
                        socket.remote_endpoint(),
                        addr_and_len,
                        ctx.as_net_context(),
                    )?;
                }
                break;
            } else if file_flags.non_blocking {
                bail_libc!(libc::EWOULDBLOCK);
            }
        }

        ctx.new_fd_from(0, &Rc::new(RefCell::new(file)), fd_flags)
    }
}

pub fn build_socket_file(
    domain: i32,
    stype: i32,
    protocol: i32,
    ctx: &dyn Context,
) -> SysResult<File> {
    let file_owner = ctx.file_owner();
    let dev = NET_DEVICE.lock().unwrap();
    let ino = dev.next_ino();
    let iops = SimpleFileInode {
        attrs: InodeSimpleAttributes::new(
            file_owner,
            FilePermissions {
                user: PermMask {
                    read: true,
                    write: true,
                    execute: false,
                },
                ..FilePermissions::default()
            },
            linux::SOCKFS_MAGIC,
            &|| ctx.now(),
        ),
    };

    let inode = Inode::new(
        Box::new(iops),
        Rc::new(MountSource::new_pseudo()),
        StableAttr {
            typ: InodeType::Socket,
            device_id: dev.device_id(),
            inode_id: ino,
            block_size: PAGE_SIZE as i64,
            device_file_major: 0,
            device_file_minor: 0,
        },
    );

    let dirent = Dirent::new(inode, format!("socket:[{}]", ino));
    let socket_file = SocketFile::new(domain, stype, protocol, dirent, ctx)?;
    let file = File::new(
        FileFlags {
            read: true,
            write: true,
            non_seekable: true,
            ..FileFlags::default()
        },
        Box::new(socket_file),
    );
    Ok(file)
}
