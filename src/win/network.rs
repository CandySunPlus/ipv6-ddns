use std::net::Ipv6Addr;
use std::ptr::null;

use ipv6_ddns::{AddressKind, Candidate, public_v6};
use windows_sys::Win32::Foundation::ERROR_BUFFER_OVERFLOW;
use windows_sys::Win32::NetworkManagement::IpHelper::*;
use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows_sys::Win32::Networking::WinSock::{
    AF_INET6, IpDadStatePreferred, IpPrefixOriginDhcp as PREFIX_DHCP,
    IpPrefixOriginManual as PREFIX_MANUAL, IpPrefixOriginRouterAdvertisement as PREFIX_RA,
    IpSuffixOriginDhcp as SUFFIX_DHCP, IpSuffixOriginRandom as SUFFIX_RANDOM, SOCKADDR_IN6,
};

use super::{Result, last_error};

pub fn candidates() -> Result<Vec<Candidate>> {
    let mut size = 15_000;
    for _ in 0..3 {
        let mut buffer = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
        let first = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        let status = unsafe {
            GetAdaptersAddresses(
                AF_INET6 as u32,
                GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER,
                null(),
                first,
                &mut size,
            )
        };
        if status == ERROR_BUFFER_OVERFLOW {
            continue;
        }
        if status == windows_sys::Win32::Foundation::ERROR_NO_DATA {
            return Ok(Vec::new());
        }
        super::win_code(status, "读取网卡 IPv6 失败")?;

        let mut result = Vec::new();
        let mut adapter = first;
        while let Some(item) = unsafe { adapter.as_ref() } {
            adapter = item.Next;
            if item.OperStatus != IfOperStatusUp {
                continue;
            }
            let metric = item.Ipv6Metric;
            let mut address = item.FirstUnicastAddress;
            while let Some(item) = unsafe { address.as_ref() } {
                address = item.Next;
                if item.DadState != IpDadStatePreferred
                    || item.PreferredLifetime == 0
                    || item.Address.iSockaddrLength < size_of::<SOCKADDR_IN6>() as i32
                {
                    continue;
                }
                let Some(socket) =
                    (unsafe { item.Address.lpSockaddr.cast::<SOCKADDR_IN6>().as_ref() })
                else {
                    continue;
                };
                let ip = Ipv6Addr::from(unsafe { socket.sin6_addr.u.Byte });
                if socket.sin6_family != AF_INET6 || !public_v6(ip) {
                    continue;
                }
                let kind = match (item.PrefixOrigin, item.SuffixOrigin) {
                    (PREFIX_DHCP, _) | (_, SUFFIX_DHCP) => AddressKind::Dhcp,
                    (PREFIX_MANUAL, _) => AddressKind::Manual,
                    (PREFIX_RA, SUFFIX_RANDOM) => AddressKind::Temporary,
                    (PREFIX_RA, _) => AddressKind::Stable,
                    _ => AddressKind::Other,
                };
                result.push(Candidate {
                    address: ip,
                    kind,
                    metric,
                    preferred_lifetime: item.PreferredLifetime,
                });
            }
        }
        return Ok(result);
    }
    Err("网卡正在变化，请稍后重试。".into())
}

pub fn domain(input: &str) -> Result<String> {
    use windows_sys::Win32::Globalization::{IDN_USE_STD3_ASCII_RULES, IdnToAscii};
    let source: Vec<_> = input.trim().encode_utf16().collect();
    if source.is_empty() || source.len() > 253 {
        return Err("请填写完整域名，例如 home.example.com。".into());
    }
    let mut output = [0; 254];
    let count = unsafe {
        IdnToAscii(
            IDN_USE_STD3_ASCII_RULES,
            source.as_ptr(),
            source.len() as i32,
            output.as_mut_ptr(),
            output.len() as i32,
        )
    };
    if count == 0 {
        return Err(last_error("域名格式无效"));
    }
    ipv6_ddns::dns_name(&String::from_utf16_lossy(&output[..count as usize])).map_err(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_ascii_and_internationalized_domain_names() {
        assert_eq!(domain(" Home.Example.COM. ").unwrap(), "home.example.com");
        assert_eq!(domain("bücher.example").unwrap(), "xn--bcher-kva.example");
        assert!(domain("https://example.com").is_err());
    }
}
