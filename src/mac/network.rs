use std::ffi::{CString, c_char, c_int, c_void};
use std::mem::size_of;
use std::net::Ipv6Addr;
use std::ptr::null_mut;

use ipv6_ddns::{AddressKind, Candidate, public_v6};

use super::{Result, error};

const IN6_IFF_ANYCAST: c_int = 0x0001;
const IN6_IFF_TENTATIVE: c_int = 0x0002;
const IN6_IFF_DUPLICATED: c_int = 0x0004;
const IN6_IFF_DETACHED: c_int = 0x0008;
const IN6_IFF_DEPRECATED: c_int = 0x0010;
const IN6_IFF_AUTOCONF: c_int = 0x0040;
const IN6_IFF_TEMPORARY: c_int = 0x0080;
const IN6_IFF_DYNAMIC: c_int = 0x0100;
const IN6_IFF_OPTIMISTIC: c_int = 0x0200;

const SIOCGIFALIFETIME_IN6: libc::c_ulong = 0xc000_0000
    | ((size_of::<libc::in6_ifreq>() as libc::c_ulong & 0x1fff) << 16)
    | (('i' as libc::c_ulong) << 8)
    | 81;

struct IfAddrs(*mut libc::ifaddrs);

impl Drop for IfAddrs {
    fn drop(&mut self) {
        unsafe {
            libc::freeifaddrs(self.0);
        }
    }
}

fn name_of(entry: &libc::ifaddrs) -> &[u8] {
    if entry.ifa_name.is_null() {
        return &[];
    }
    unsafe { std::ffi::CStr::from_ptr(entry.ifa_name).to_bytes() }
}

fn metric(list: *mut libc::ifaddrs, name: &[u8]) -> u32 {
    let mut current = list;
    while !current.is_null() {
        let item = unsafe { &*current };
        current = item.ifa_next;
        if name_of(item) == name
            && !item.ifa_addr.is_null()
            && unsafe { (*item.ifa_addr).sa_family as c_int } == libc::AF_LINK
            && !item.ifa_data.is_null()
        {
            return unsafe { (*item.ifa_data.cast::<libc::if_data>()).ifi_metric };
        }
    }
    0
}

fn request(
    socket: c_int,
    name: &[u8],
    address: &libc::sockaddr_in6,
    code: libc::c_ulong,
) -> Option<libc::in6_ifreq> {
    let mut buffer: libc::in6_ifreq = unsafe { std::mem::zeroed() };
    let length = name.len().min(buffer.ifr_name.len() - 1);
    for (index, byte) in name[..length].iter().enumerate() {
        buffer.ifr_name[index] = *byte as c_char;
    }
    buffer.ifr_ifru.ifru_addr = *address;
    (unsafe { libc::ioctl(socket, code, &mut buffer) } == 0).then_some(buffer)
}

pub fn candidates() -> Result<Vec<Candidate>> {
    let mut raw = null_mut();
    if unsafe { libc::getifaddrs(&mut raw) } != 0 {
        return Err(error("读取网卡 IPv6 失败", std::io::Error::last_os_error()));
    }
    let list = IfAddrs(raw);
    let socket = unsafe { libc::socket(libc::AF_INET6, libc::SOCK_DGRAM, 0) };
    if socket < 0 {
        return Err(error(
            "打开 IPv6 套接字失败",
            std::io::Error::last_os_error(),
        ));
    }

    let mut result = Vec::new();
    let mut current = list.0;
    while !current.is_null() {
        let item = unsafe { &*current };
        current = item.ifa_next;
        if item.ifa_addr.is_null()
            || unsafe { (*item.ifa_addr).sa_family as c_int } != libc::AF_INET6
            || item.ifa_flags & libc::IFF_UP as u32 == 0
            || item.ifa_flags & libc::IFF_LOOPBACK as u32 != 0
        {
            continue;
        }
        let address = unsafe { *item.ifa_addr.cast::<libc::sockaddr_in6>() };
        let ip = Ipv6Addr::from(address.sin6_addr.s6_addr);
        if !public_v6(ip) {
            continue;
        }
        let name = name_of(item);
        let Some(flags) = request(socket, name, &address, libc::SIOCGIFAFLAG_IN6) else {
            continue;
        };
        let flags = unsafe { flags.ifr_ifru.ifru_flags6 };
        if flags
            & (IN6_IFF_ANYCAST
                | IN6_IFF_TENTATIVE
                | IN6_IFF_DUPLICATED
                | IN6_IFF_DETACHED
                | IN6_IFF_DEPRECATED
                | IN6_IFF_OPTIMISTIC)
            != 0
        {
            continue;
        }
        let preferred_lifetime = request(socket, name, &address, SIOCGIFALIFETIME_IN6)
            .map(|value| unsafe { value.ifr_ifru.ifru_lifetime.ia6t_pltime })
            .unwrap_or(u32::MAX);
        if preferred_lifetime == 0 {
            continue;
        }
        let kind = if flags & IN6_IFF_DYNAMIC != 0 {
            AddressKind::Dhcp
        } else if flags & IN6_IFF_TEMPORARY != 0 {
            AddressKind::Temporary
        } else if flags & IN6_IFF_AUTOCONF != 0 {
            AddressKind::Stable
        } else {
            AddressKind::Manual
        };
        let metric = metric(list.0, name);
        let metric = if metric == 0 && !item.ifa_name.is_null() {
            unsafe { libc::if_nametoindex(item.ifa_name) }
        } else {
            metric
        };
        result.push(Candidate {
            address: ip,
            kind,
            metric,
            preferred_lifetime,
        });
    }
    unsafe {
        libc::close(socket);
    }
    Ok(result)
}

#[repr(C)]
struct IdnaInfo {
    size: i16,
    is_transitional_different: i8,
    reserved_b3: i8,
    errors: u32,
    reserved_i2: i32,
    reserved_i3: i32,
}

#[link(name = "icucore")]
unsafe extern "C" {
    fn uidna_openUTS46(options: u32, status: *mut i32) -> *mut c_void;
    fn uidna_nameToASCII_UTF8(
        idna: *const c_void,
        name: *const c_char,
        length: i32,
        dest: *mut c_char,
        capacity: i32,
        info: *mut IdnaInfo,
        status: *mut i32,
    ) -> i32;
    fn uidna_close(idna: *mut c_void);
}

const UIDNA_USE_STD3_RULES: u32 = 2;
const INVALID_DOMAIN: &str = "请填写完整域名，例如 home.example.com。";

pub fn domain(input: &str) -> Result<String> {
    let input = input.trim();
    if input.is_empty() || input.len() > 253 {
        return Err(INVALID_DOMAIN.into());
    }
    let source = CString::new(input).map_err(|_| INVALID_DOMAIN.to_owned())?;
    let mut status: i32 = 0;
    let idna = unsafe { uidna_openUTS46(UIDNA_USE_STD3_RULES, &mut status) };
    if idna.is_null() || status != 0 {
        return Err(INVALID_DOMAIN.into());
    }
    let mut output = [0 as c_char; 254];
    let mut info = IdnaInfo {
        size: size_of::<IdnaInfo>() as i16,
        is_transitional_different: 0,
        reserved_b3: 0,
        errors: 0,
        reserved_i2: 0,
        reserved_i3: 0,
    };
    let length = unsafe {
        uidna_nameToASCII_UTF8(
            idna,
            source.as_ptr(),
            -1,
            output.as_mut_ptr(),
            output.len() as i32,
            &mut info,
            &mut status,
        )
    };
    unsafe {
        uidna_close(idna);
    }
    if status != 0 || info.errors != 0 || length <= 0 {
        return Err(INVALID_DOMAIN.into());
    }
    let bytes: Vec<u8> = output[..length as usize]
        .iter()
        .map(|byte| *byte as u8)
        .collect();
    let name = String::from_utf8(bytes).map_err(|_| INVALID_DOMAIN.to_owned())?;
    ipv6_ddns::dns_name(&name).map_err(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_ascii_and_internationalized_domain_names() {
        assert_eq!(domain(" Home.Example.COM. ").unwrap(), "home.example.com");
        assert_eq!(
            domain("中文.example.com").unwrap(),
            "xn--fiq228c.example.com"
        );
        for invalid in ["", "example", "a..example.com", "-a.example.com", "1.2.3.4"] {
            assert!(domain(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn reads_ipv6_candidates_without_panicking() {
        let candidates = candidates().unwrap();
        for candidate in candidates {
            assert!(public_v6(candidate.address));
            assert_ne!(candidate.preferred_lifetime, 0);
        }
    }
}
