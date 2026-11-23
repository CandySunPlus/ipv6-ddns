use std::io;
use std::net::Ipv6Addr;
use std::os::fd::AsRawFd;

use libc::{c_char, c_int, c_ulong, in6_addrlifetime, sockaddr_in6, time_t};

use super::{
    IN6_IFF_DEPRECATED, IN6_IFF_TEMPORARY, IfAddrs, SIOCGIFALIFETIME_IN6, UNUSABLE, interface_name,
    ipv6_of, ipv6_socket, name_of, public_v6, request,
};

#[repr(C)]
struct In6AliasReq {
    name: [c_char; libc::IFNAMSIZ],
    addr: sockaddr_in6,
    dstaddr: sockaddr_in6,
    prefixmask: sockaddr_in6,
    flags: c_int,
    lifetime: in6_addrlifetime,
}

// Darwin <netinet6/in6_var.h>. SIOCSIFALIFETIME_IN6 overlays its address and
// lifetime in the same union, whereas the alias request supplies both.
const SIOCAIFADDR_IN6: c_ulong = 0x8000_0000
    | ((size_of::<In6AliasReq>() as c_ulong & 0x1fff) << 16)
    | (('i' as c_ulong) << 8)
    | 26;
const SIOCGIFNETMASK_IN6: c_ulong = (SIOCGIFALIFETIME_IN6 & !0xff) | 37;

#[derive(Clone, Copy)]
struct Address<'a> {
    name: &'a [u8],
    addr: &'a sockaddr_in6,
    up: bool,
}

impl Address<'_> {
    fn flags(self, socket: c_int) -> io::Result<c_int> {
        let flags = request(socket, self.name, self.addr, libc::SIOCGIFAFLAG_IN6)?;
        Ok(unsafe { flags.ifr_ifru.ifru_flags6 })
    }

    fn lifetime(self, socket: c_int) -> io::Result<in6_addrlifetime> {
        let info = request(socket, self.name, self.addr, SIOCGIFALIFETIME_IN6)?;
        Ok(unsafe { info.ifr_ifru.ifru_lifetime })
    }

    fn set_preferred(
        self,
        socket: c_int,
        flags: c_int,
        lifetime: in6_addrlifetime,
        preferred: u32,
    ) -> io::Result<()> {
        // Use the remaining validity, not the original duration: XNU treats
        // this write as a fresh countdown and ignores the absolute deadlines.
        let valid = remaining(lifetime.ia6t_expire, lifetime.ia6t_vltime, wall_time());
        if valid == 0 {
            return Err(io::Error::from_raw_os_error(libc::EADDRNOTAVAIL));
        }
        // This read also verifies that the address still exists just before
        // issuing the add-or-update ioctl. There is no existing-only mode.
        let mask = request(socket, self.name, self.addr, SIOCGIFNETMASK_IN6)?;
        let update = In6AliasReq {
            name: interface_name(self.name),
            addr: *self.addr,
            dstaddr: unsafe { std::mem::zeroed() },
            prefixmask: unsafe { mask.ifr_ifru.ifru_addr },
            flags: flags & !IN6_IFF_DEPRECATED,
            lifetime: in6_addrlifetime {
                ia6t_expire: 0,
                ia6t_preferred: 0,
                ia6t_vltime: valid,
                ia6t_pltime: preferred.min(valid),
            },
        };
        if unsafe { libc::ioctl(socket, SIOCAIFADDR_IN6, &update) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

fn addresses(list: &IfAddrs) -> impl Iterator<Item = Address<'_>> {
    list.entries().filter_map(|entry| {
        let name = name_of(entry);
        let addr = ipv6_of(entry)?;
        if name.is_empty()
            || entry.ifa_flags & (libc::IFF_LOOPBACK | libc::IFF_POINTOPOINT) as u32 != 0
            || !public_v6(Ipv6Addr::from(addr.sin6_addr.s6_addr))
        {
            return None;
        }
        Some(Address {
            name,
            addr,
            up: entry.ifa_flags & libc::IFF_UP as u32 != 0,
        })
    })
}

fn wall_time() -> time_t {
    unsafe { libc::time(std::ptr::null_mut()) }
}

fn remaining(deadline: time_t, duration: u32, now: time_t) -> u32 {
    if duration == u32::MAX {
        return duration;
    }
    (deadline.saturating_sub(now).clamp(0, i64::from(duration))) as u32
}

fn preferred(life: in6_addrlifetime, now: time_t) -> bool {
    remaining(life.ia6t_expire, life.ia6t_vltime, now) != 0
        && remaining(life.ia6t_preferred, life.ia6t_pltime, now) != 0
}

fn gone(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EADDRNOTAVAIL | libc::ENXIO)
    )
}

fn is_peer(name: &[u8], ip: &[u8; 16], interface: &[u8], target: &[u8; 16]) -> bool {
    name == interface && ip != target && ip[..8] == target[..8]
}

fn target_on(list: &IfAddrs, socket: c_int, target: [u8; 16]) -> io::Result<Option<Address<'_>>> {
    let mut matches = addresses(list).filter(|a| a.up && a.addr.sin6_addr.s6_addr == target);
    let Some(address) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() {
        return Ok(None);
    }
    let flags = address.flags(socket)?;
    let life = address.lifetime(socket)?;
    Ok(
        (flags & (UNUSABLE | IN6_IFF_DEPRECATED | IN6_IFF_TEMPORARY) == 0
            && preferred(life, wall_time()))
        .then_some(address),
    )
}

struct Change {
    interface: [c_char; libc::IFNAMSIZ],
    ip: [u8; 16],
    original_until: time_t,
    original_duration: u32,
    deprecated_at: time_t,
}

impl Change {
    fn matches(&self, address: Address<'_>) -> bool {
        self.ip == address.addr.sin6_addr.s6_addr && self.interface == interface_name(address.name)
    }

    fn restore(&mut self, socket: c_int, address: Address<'_>) -> io::Result<()> {
        let mut life = address.lifetime(socket)?;
        if life.ia6t_pltime != 0 || life.ia6t_preferred != self.deprecated_at {
            // The network (or another program) has already superseded our change.
            return Ok(());
        }
        let flags = address.flags(socket)?;
        if flags & UNUSABLE != 0 {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        let preferred = remaining(self.original_until, self.original_duration, wall_time());
        if preferred == 0 {
            return Ok(());
        }
        address.set_preferred(socket, flags, life, preferred)?;
        if flags & IN6_IFF_DEPRECATED != 0 {
            // XNU can clear the old deprecated flag but still force pltime=0
            // on the first write. Only retry if the address is still at zero.
            life = address.lifetime(socket)?;
            if life.ia6t_pltime == 0 {
                self.deprecated_at = life.ia6t_preferred;
                let flags = address.flags(socket)?;
                if flags & (UNUSABLE | IN6_IFF_DEPRECATED) != 0 {
                    return Err(io::Error::from(io::ErrorKind::WouldBlock));
                }
                let preferred = remaining(self.original_until, self.original_duration, wall_time());
                if preferred != 0 {
                    address.set_preferred(socket, flags, life, preferred)?;
                }
            }
        }
        Ok(())
    }
}

#[derive(Default)]
pub(in crate::mac) struct SourcePreference {
    changes: Vec<Change>,
}

impl SourcePreference {
    pub(in crate::mac) fn reconcile(&mut self, target: Option<Ipv6Addr>) -> io::Result<()> {
        if target.is_none() && self.changes.is_empty() {
            return Ok(());
        }
        let list = IfAddrs::read()?;
        let fd = ipv6_socket()?;
        let socket = fd.as_raw_fd();
        let scope = match target.map(|ip| target_on(&list, socket, ip.octets())) {
            Some(Ok(scope)) => scope,
            Some(Err(e)) if !gone(&e) => {
                self.restore_except(&list, socket, None)?;
                return Err(e);
            }
            _ => None,
        };
        self.restore_except(&list, socket, scope)?;
        if let Some(scope) = scope {
            let target = scope.addr.sin6_addr.s6_addr;
            for address in addresses(&list)
                .filter(|a| a.up && is_peer(a.name, &a.addr.sin6_addr.s6_addr, scope.name, &target))
            {
                if let Err(cause) = self.deprecate(socket, address)
                    && !gone(&cause)
                {
                    // Do not leave a partially applied preference after a write failure.
                    self.restore_except(&list, socket, None)?;
                    return Err(cause);
                }
            }
        }
        Ok(())
    }

    fn deprecate(&mut self, socket: c_int, address: Address<'_>) -> io::Result<()> {
        let flags = address.flags(socket)?;
        let life = address.lifetime(socket)?;
        let now = wall_time();
        if flags & UNUSABLE != 0 || !preferred(life, now) {
            return Ok(());
        }
        self.changes.retain(|change| !change.matches(address));
        self.changes.push(Change {
            interface: interface_name(address.name),
            ip: address.addr.sin6_addr.s6_addr,
            original_until: life.ia6t_preferred,
            original_duration: life.ia6t_pltime,
            deprecated_at: now,
        });
        address.set_preferred(socket, flags, life, 0)?;
        let observed = address.lifetime(socket)?;
        if observed.ia6t_pltime == 0 {
            self.changes.last_mut().unwrap().deprecated_at = observed.ia6t_preferred;
        } else {
            // A concurrent address refresh made it preferred again.
            self.changes.pop();
        }
        Ok(())
    }

    fn restore_except(
        &mut self,
        list: &IfAddrs,
        socket: c_int,
        keep: Option<Address<'_>>,
    ) -> io::Result<()> {
        let now = wall_time();
        let mut error = None;
        self.changes.retain_mut(|change| {
            if remaining(change.original_until, change.original_duration, now) == 0 {
                return false;
            }
            let Some(address) = addresses(list).find(|a| change.matches(*a)) else {
                return false;
            };
            if keep.is_some_and(|target| {
                address.up
                    && is_peer(
                        address.name,
                        &change.ip,
                        target.name,
                        &target.addr.sin6_addr.s6_addr,
                    )
            }) {
                return true;
            }
            match change.restore(socket, address) {
                Ok(()) => false,
                Err(e) if gone(&e) => false,
                Err(e) => {
                    error.get_or_insert(e);
                    true
                }
            }
        });
        error.map_or(Ok(()), Err)
    }
}
