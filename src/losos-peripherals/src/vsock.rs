//! vsock streams, which std has no type for, as Files: a connected socket
//! reads and writes like one, and USB/IP's kernel halves take it by its
//! descriptor.
//!
//! Only the host connects. A machine listens, and can be reached by the
//! host alone, since vsock carries nothing from one machine to another.
//! On the host anyone may connect to a machine, though, so the host binds
//! its end to a port below 1024, which only root can, and the machine's
//! listener drops a connection from any other: a person signed in to the
//! host cannot ask the USB machine for someone's device.

use std::fs::File;
use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// The host's address, as a machine sees it.
pub const HOST: u32 = 2;
/// Ports below this only root can bind.
const PRIVILEGED: u32 = 1024;

fn addr(cid: u32, port: u32) -> libc::sockaddr_vm {
    let mut a: libc::sockaddr_vm = unsafe { mem::zeroed() };
    a.svm_family = libc::AF_VSOCK as libc::sa_family_t;
    a.svm_cid = cid;
    a.svm_port = port;
    a
}

fn socket() -> io::Result<OwnedFd> {
    let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn bind(fd: &OwnedFd, a: &libc::sockaddr_vm) -> io::Result<()> {
    let r = unsafe {
        libc::bind(
            fd.as_raw_fd(),
            a as *const _ as *const libc::sockaddr,
            mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
        )
    };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Connects to `port` of machine `cid` from a privileged port of the
/// host's, trying them from the top down until one is free.
pub fn connect(cid: u32, port: u32) -> io::Result<File> {
    let fd = socket()?;
    let mut bound = false;
    for local in (600..PRIVILEGED).rev() {
        match bind(&fd, &addr(libc::VMADDR_CID_ANY, local)) {
            Ok(()) => {
                bound = true;
                break;
            }
            Err(e) if e.raw_os_error() == Some(libc::EADDRINUSE) => continue,
            Err(e) => return Err(e),
        }
    }
    if !bound {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "no privileged vsock port is free",
        ));
    }
    let a = addr(cid, port);
    let r = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            &a as *const _ as *const libc::sockaddr,
            mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
        )
    };
    if r < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(File::from(fd))
}

/// A machine's listener, which takes connections from the host's root
/// alone.
pub struct Listener(OwnedFd);

impl Listener {
    pub fn bind(port: u32) -> io::Result<Listener> {
        let fd = socket()?;
        bind(&fd, &addr(libc::VMADDR_CID_ANY, port))?;
        if unsafe { libc::listen(fd.as_raw_fd(), 8) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Listener(fd))
    }

    /// The next connection from the host's root; any other is closed.
    pub fn accept(&self) -> io::Result<File> {
        loop {
            let mut a: libc::sockaddr_vm = unsafe { mem::zeroed() };
            let mut len = mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t;
            let fd = unsafe {
                libc::accept4(
                    self.0.as_raw_fd(),
                    &mut a as *mut _ as *mut libc::sockaddr,
                    &mut len,
                    libc::SOCK_CLOEXEC,
                )
            };
            if fd < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            let conn = unsafe { OwnedFd::from_raw_fd(fd) };
            if a.svm_cid == HOST && a.svm_port < PRIVILEGED {
                return Ok(File::from(conn));
            }
            tracing::warn!(
                cid = a.svm_cid,
                port = a.svm_port,
                "refused a connection not from the host's root"
            );
        }
    }
}
