use std::net::Ipv6Addr;
use std::ptr::{addr_of, null_mut};
use std::time::{Duration, Instant};

use ipv6_ddns::public_v6;
use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_NOT_FOUND};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetIfEntry2, GetUnicastIpAddressEntry, GetUnicastIpAddressTable, MIB_IF_ROW2,
    MIB_UNICASTIPADDRESS_ROW, MIB_UNICASTIPADDRESS_TABLE, SetUnicastIpAddressEntry,
};
use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows_sys::Win32::Networking::WinSock::{
    AF_INET6, IpDadStatePreferred, IpPrefixOriginUnchanged, IpSuffixOriginUnchanged,
};

use super::{Result, win_code};

type Row = MIB_UNICASTIPADDRESS_ROW;
const INFINITE: u32 = u32::MAX;

struct AddressTable(*mut MIB_UNICASTIPADDRESS_TABLE);

impl AddressTable {
    fn read() -> Result<Self> {
        let mut table = null_mut();
        win_code(
            unsafe { GetUnicastIpAddressTable(AF_INET6, &mut table) },
            "读取 IPv6 首选期失败",
        )?;
        Ok(Self(table))
    }

    fn rows(&self) -> &[Row] {
        if self.0.is_null() {
            return &[];
        }
        // The system allocation extends the trailing array to NumEntries rows.
        unsafe {
            std::slice::from_raw_parts(
                addr_of!((*self.0).Table).cast::<Row>(),
                (*self.0).NumEntries as usize,
            )
        }
    }
}

impl Drop for AddressTable {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { FreeMibTable(self.0.cast()) };
        }
    }
}

fn address(row: &Row) -> Ipv6Addr {
    // All rows used here come from the IPv6-only table or a lookup using one of its rows.
    Ipv6Addr::from(unsafe { row.Address.Ipv6.sin6_addr.u.Byte })
}

fn interface(row: &Row) -> u64 {
    unsafe { row.InterfaceLuid.Value }
}

fn same_address(a: &Row, b: &Row) -> bool {
    interface(a) == interface(b) && address(a) == address(b)
}

fn same_instance(a: &Row, b: &Row) -> bool {
    same_address(a, b) && a.CreationTimeStamp == b.CreationTimeStamp
}

fn preferred(row: &Row) -> bool {
    row.DadState == IpDadStatePreferred
        && row.ValidLifetime != 0
        && row.PreferredLifetime != 0
        && !row.SkipAsSource
}

fn peer(row: &Row, target: &Row) -> bool {
    let ip = address(row);
    let target_ip = address(target);
    // Windows can report DHCPv6 and temporary addresses as /128 on a shared /64.
    ip != target_ip
        && interface(row) == interface(target)
        && ip.to_bits() >> 64 == target_ip.to_bits() >> 64
        && public_v6(ip)
}

fn interface_up(row: &Row) -> Result<bool> {
    let mut link = MIB_IF_ROW2 {
        InterfaceLuid: row.InterfaceLuid,
        ..Default::default()
    };
    let status = unsafe { GetIfEntry2(&mut link) };
    if missing(status) {
        return Ok(false);
    }
    win_code(status, "读取 IPv6 网卡状态失败")?;
    Ok(link.OperStatus == IfOperStatusUp)
}

fn target_row(rows: &[Row], ip: Ipv6Addr) -> Result<Option<Row>> {
    let mut target = None;
    for row in rows {
        if address(row) == ip && preferred(row) && interface_up(row)? {
            // The published address alone cannot distinguish two active interfaces.
            if target.is_some() {
                return Ok(None);
            }
            target = Some(*row);
        }
    }
    Ok(target)
}

fn missing(status: u32) -> bool {
    matches!(status, ERROR_NOT_FOUND | ERROR_FILE_NOT_FOUND)
}

fn read_current(mut row: Row) -> Result<Option<Row>> {
    let status = unsafe { GetUnicastIpAddressEntry(&mut row) };
    if missing(status) {
        return Ok(None);
    }
    win_code(status, "读取当前 IPv6 首选期失败")?;
    Ok(Some(row))
}

fn set_preferred(mut row: Row, lifetime: u32) -> Result<bool> {
    // Let Windows retain the existing origins of DHCPv6 and SLAAC addresses.
    row.PrefixOrigin = IpPrefixOriginUnchanged;
    row.SuffixOrigin = IpSuffixOriginUnchanged;
    row.PreferredLifetime = lifetime.min(row.ValidLifetime);
    let status = unsafe { SetUnicastIpAddressEntry(&row) };
    if missing(status) {
        return Ok(false);
    }
    if status == ERROR_ACCESS_DENIED {
        return Err("调整 IPv6 首选期需要以 LocalSystem 运行 DDNS 服务。".into());
    }
    win_code(status, "调整 IPv6 首选期失败")?;
    Ok(true)
}

struct Change {
    original: Row,
    observed_at: Instant,
}

impl Change {
    fn remaining(&self, now: Instant) -> u32 {
        let original = self.original.PreferredLifetime;
        if original == INFINITE {
            return INFINITE;
        }
        let elapsed = now.saturating_duration_since(self.observed_at);
        Duration::from_secs(u64::from(original))
            .saturating_sub(elapsed)
            .as_secs() as u32
    }

    fn restore(&self) -> Result<()> {
        let Some(row) = read_current(self.original)? else {
            return Ok(());
        };
        let remaining = self.remaining(Instant::now());
        if same_instance(&row, &self.original)
            && row.PreferredLifetime == 0
            && row.ValidLifetime != 0
            && row.OnLinkPrefixLength == self.original.OnLinkPrefixLength
            && row.PrefixOrigin == self.original.PrefixOrigin
            && row.SuffixOrigin == self.original.SuffixOrigin
            && row.SkipAsSource == self.original.SkipAsSource
            && remaining != 0
        {
            set_preferred(row, remaining)?;
        }
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct SourcePreference {
    changes: Vec<Change>,
}

impl SourcePreference {
    pub(super) fn reconcile(&mut self, target: Option<Ipv6Addr>) -> Result<()> {
        let Some(target) = target else {
            return self.restore_except(None);
        };
        let result = self.apply(target);
        if let Err(cause) = result {
            // Roll back successful writes if a later write or address lookup fails.
            if let Err(restore) = self.restore_except(None) {
                return Err(format!("{cause}。恢复 IPv6 首选期失败（{restore}）"));
            }
            return Err(cause);
        }
        Ok(())
    }

    fn apply(&mut self, target: Ipv6Addr) -> Result<()> {
        let table = AddressTable::read()?;
        let rows = table.rows();
        let target = target_row(rows, target)?;
        let target = match target {
            Some(target) => read_current(target)?
                .filter(|current| same_instance(current, &target) && preferred(current)),
            None => None,
        };
        self.restore_except(target.as_ref())?;
        let Some(target) = target else {
            return Ok(());
        };
        self.changes.retain(|change| {
            rows.iter().any(|row| same_instance(row, &change.original))
                && change.remaining(Instant::now()) != 0
        });
        for peer_row in rows
            .iter()
            .filter(|row| peer(row, &target) && preferred(row))
        {
            let Some(row) = read_current(*peer_row)? else {
                continue;
            };
            if !same_instance(&row, peer_row) || !peer(&row, &target) || !preferred(&row) {
                continue;
            }
            // A new nonzero lifetime supersedes any earlier change to this address.
            self.changes
                .retain(|change| !same_address(&change.original, &row));
            let observed_at = Instant::now();
            if set_preferred(row, 0)? {
                self.changes.push(Change {
                    original: row,
                    observed_at,
                });
            }
        }
        Ok(())
    }

    fn restore_except(&mut self, target: Option<&Row>) -> Result<()> {
        let mut error = None;
        self.changes.retain(|change| {
            if change.remaining(Instant::now()) == 0 {
                return false;
            }
            if target.is_some_and(|target| peer(&change.original, target)) {
                return true;
            }
            if let Err(cause) = change.restore() {
                error.get_or_insert(cause);
                return true;
            }
            false
        });
        error.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::NetworkManagement::Ndis::NET_LUID_LH;
    use windows_sys::Win32::Networking::WinSock::{
        IN6_ADDR, IN6_ADDR_0, SOCKADDR_IN6, SOCKADDR_INET,
    };

    fn ipv6_row(ip: &str, luid: u64, prefix: u8) -> Row {
        Row {
            Address: SOCKADDR_INET {
                Ipv6: SOCKADDR_IN6 {
                    sin6_family: AF_INET6,
                    sin6_addr: IN6_ADDR {
                        u: IN6_ADDR_0 {
                            Byte: ip.parse::<Ipv6Addr>().unwrap().octets(),
                        },
                    },
                    ..Default::default()
                },
            },
            InterfaceLuid: NET_LUID_LH { Value: luid },
            OnLinkPrefixLength: prefix,
            DadState: IpDadStatePreferred,
            PreferredLifetime: 120,
            ValidLifetime: 300,
            ..Default::default()
        }
    }

    #[test]
    fn deprecation_is_scoped_to_other_global_addresses_on_the_same_link_and_64() {
        let target = ipv6_row("2408:1234:1:2::1", 7, 128);
        assert!(peer(&ipv6_row("2408:1234:1:2::2", 7, 128), &target));
        assert!(peer(&ipv6_row("2408:1234:1:2::2", 7, 64), &target));
        for row in [
            target,
            ipv6_row("2408:1234:1:2::2", 8, 128),
            ipv6_row("2408:1234:1:3::2", 7, 128),
            ipv6_row("fe80::2", 7, 64),
        ] {
            assert!(!peer(&row, &target));
        }
    }

    #[test]
    fn restoration_does_not_extend_the_original_preferred_lifetime() {
        let now = Instant::now();
        let mut change = Change {
            original: ipv6_row("2408:1234:1:2::2", 7, 64),
            observed_at: now,
        };
        assert_eq!(change.remaining(now + Duration::from_secs(20)), 100);
        assert_eq!(change.remaining(now + Duration::from_secs(125)), 0);
        change.original.PreferredLifetime = INFINITE;
        assert_eq!(change.remaining(now + Duration::from_secs(125)), INFINITE);
    }

    #[test]
    fn reads_native_ipv6_addresses_without_modifying_them() {
        let table = AddressTable::read().unwrap();
        let loopback = table
            .rows()
            .iter()
            .find(|row| address(row) == Ipv6Addr::LOCALHOST)
            .expect("IPv6 loopback address");
        let current = read_current(*loopback).unwrap().unwrap();
        assert!(same_instance(loopback, &current));
    }
}
