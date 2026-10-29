//! A UDP socket on a well-known multicast port (mDNS 5353, SSDP 1900) that
//! coexists with the system's own responders (Avahi, mDNSResponder, Windows):
//! the reuse options must be set before binding, which the standard library
//! does not offer.

use std::net::UdpSocket;

#[cfg(unix)]
pub fn bind_shared_v4(port: u16) -> std::io::Result<UdpSocket> {
    use std::os::fd::FromRawFd;
    use std::os::raw::{c_int, c_void};

    extern "C" {
        fn socket(domain: c_int, ty: c_int, protocol: c_int) -> c_int;
        fn setsockopt(
            fd: c_int,
            level: c_int,
            name: c_int,
            value: *const c_void,
            len: u32,
        ) -> c_int;
        fn bind(fd: c_int, addr: *const c_void, len: u32) -> c_int;
        fn close(fd: c_int) -> c_int;
    }

    const AF_INET: c_int = 2;
    const SOCK_DGRAM: c_int = 2;
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    const OPTS: (c_int, c_int, c_int) = (0xffff, 0x4, 0x200);
    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "freebsd")))]
    const OPTS: (c_int, c_int, c_int) = (1, 2, 15);
    let (sol_socket, so_reuseaddr, so_reuseport) = OPTS;

    // sockaddr_in for 0.0.0.0:port.
    let mut addr = [0u8; 16];
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    {
        addr[0] = 16;
        addr[1] = AF_INET as u8;
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "freebsd")))]
    addr[..2].copy_from_slice(&(AF_INET as u16).to_ne_bytes());
    addr[2..4].copy_from_slice(&port.to_be_bytes());

    // SAFETY: plain socket calls on a descriptor owned here; on success it
    // is handed to UdpSocket, which then owns and closes it.
    unsafe {
        let fd = socket(AF_INET, SOCK_DGRAM, 0);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let one: c_int = 1;
        let size = std::mem::size_of::<c_int>() as u32;
        let p = &one as *const c_int as *const c_void;
        setsockopt(fd, sol_socket, so_reuseaddr, p, size);
        setsockopt(fd, sol_socket, so_reuseport, p, size);
        if bind(fd, addr.as_ptr() as *const c_void, addr.len() as u32) != 0 {
            let e = std::io::Error::last_os_error();
            close(fd);
            return Err(e);
        }
        Ok(UdpSocket::from_raw_fd(fd))
    }
}

#[cfg(windows)]
pub fn bind_shared_v4(port: u16) -> std::io::Result<UdpSocket> {
    use std::os::windows::io::FromRawSocket;

    #[link(name = "ws2_32")]
    extern "system" {
        fn socket(af: i32, ty: i32, protocol: i32) -> usize;
        fn setsockopt(s: usize, level: i32, name: i32, value: *const u8, len: i32) -> i32;
        fn bind(s: usize, addr: *const u8, len: i32) -> i32;
        fn closesocket(s: usize) -> i32;
        fn WSAGetLastError() -> i32;
    }

    // Makes the standard library initialise Winsock first.
    drop(UdpSocket::bind("127.0.0.1:0")?);
    let mut addr = [0u8; 16];
    addr[..2].copy_from_slice(&2u16.to_ne_bytes());
    addr[2..4].copy_from_slice(&port.to_be_bytes());
    // SAFETY: as above; the socket is handed to UdpSocket on success.
    unsafe {
        let s = socket(2, 2, 0);
        if s == usize::MAX {
            return Err(std::io::Error::from_raw_os_error(WSAGetLastError()));
        }
        let one: i32 = 1;
        setsockopt(s, 0xffff, 4, &one as *const i32 as *const u8, 4);
        if bind(s, addr.as_ptr(), addr.len() as i32) != 0 {
            let e = std::io::Error::from_raw_os_error(WSAGetLastError());
            closesocket(s);
            return Err(e);
        }
        Ok(UdpSocket::from_raw_socket(s as u64))
    }
}

#[cfg(not(any(unix, windows)))]
pub fn bind_shared_v4(port: u16) -> std::io::Result<UdpSocket> {
    UdpSocket::bind(("0.0.0.0", port))
}
