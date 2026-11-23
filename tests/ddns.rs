use ipv6_ddns::{
    AddressKind, AddressPolicy, Candidate, Ipv6Suffix, Settings, cloudflare, dns_name, public_v6,
    select_address,
};
use serde_json::json;

fn candidate(address: &str, kind: AddressKind, metric: u32) -> Candidate {
    Candidate {
        address: address.parse().unwrap(),
        kind,
        metric,
        preferred_lifetime: 1_000,
    }
}

#[test]
fn defaults_to_a_stable_address_on_the_preferred_interface() {
    let candidates = [
        candidate("2408:1::1234", AddressKind::Temporary, 5),
        candidate("2408:1::8023", AddressKind::Dhcp, 5),
        candidate("2408:2::1234", AddressKind::Stable, 25),
        candidate("2408:1::2345", AddressKind::Stable, 10),
        candidate("fd00::1", AddressKind::Manual, 1),
    ];
    assert_eq!(
        select_address(&AddressPolicy::Stable, &candidates).unwrap(),
        candidates[3].address
    );
    assert!(select_address(&AddressPolicy::Stable, &candidates[..2]).is_err());
    let manual = candidate("2408:1::100", AddressKind::Manual, 10);
    assert_eq!(
        select_address(
            &AddressPolicy::Stable,
            &[candidates[3].clone(), manual.clone()]
        )
        .unwrap(),
        manual.address
    );
}

#[test]
fn matches_suffix_by_numeric_segments_even_after_a_prefix_change() {
    let suffix = Ipv6Suffix::parse(" ::00AB:0023 ").unwrap();
    assert_eq!(suffix.to_string(), "::ab:23");
    let policy = AddressPolicy::Suffix(suffix);
    let candidates = [
        candidate("2408:1::ab:123", AddressKind::Stable, 10),
        candidate("2408:1::ab:23", AddressKind::Temporary, 10),
        candidate("2408:2::ab:23", AddressKind::Dhcp, 20),
        candidate("2408:3::ab:23", AddressKind::Dhcp, 20),
    ];
    assert_eq!(
        select_address(&policy, &candidates[..3]).unwrap(),
        candidates[2].address
    );
    assert_eq!(
        select_address(&policy, &candidates[3..]).unwrap(),
        candidates[3].address
    );
    assert!(select_address(&policy, &candidates[..2]).is_err());
    for invalid in [
        "",
        "::",
        "::ab:*",
        "::ab::23",
        "::00000",
        "::1:2:3:4:5:6:7:8:9",
    ] {
        assert!(Ipv6Suffix::parse(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn loads_legacy_settings_as_stable_mode() {
    let settings: Settings =
        serde_json::from_value(json!({"domain": "home.example.com", "selection": {
            "adapter": "ethernet", "address": "2408:1::1", "kind": "Temporary"
        }}))
        .unwrap();
    assert_eq!(settings.domain, "home.example.com");
    assert_eq!(settings.address, AddressPolicy::Stable);
    #[cfg(target_os = "macos")]
    assert!(!settings.prefer_ddns_source);
}

#[test]
fn validates_addresses_and_domains() {
    assert!(public_v6("2408:1234::1".parse().unwrap()));
    for ip in [
        "::1",
        "fe80::1",
        "fd12::1",
        "2001:db8::1",
        "3fff::1",
        "::ffff:1.2.3.4",
    ] {
        assert!(!public_v6(ip.parse().unwrap()), "{ip}");
    }
    assert_eq!(dns_name(" Home.Example.COM. ").unwrap(), "home.example.com");
    for name in [
        "https://home.example.com",
        "1.2.3.4",
        "example..com",
        "-a.example.com",
    ] {
        assert!(dns_name(name).is_err(), "{name}");
    }
}

const ZONE: &str = "0123456789abcdef0123456789abcdef";
const RECORD: &str = "0123456789abcdef0123456789abcd01";
const DOMAIN: &str = "home.example.com";

#[test]
fn creates_dns_only_record_and_uses_exact_record_filter() {
    let ip = "2408:1234::1".parse().unwrap();
    let mut calls = Vec::new();
    cloudflare::update(DOMAIN, ip, |method, path, body| {
        calls.push((method.to_owned(), path.to_owned(), body));
        match calls.len() {
            2 => Ok(json!([{ "name": "example.com", "id": ZONE }])),
            4 => Ok(json!({"ttl": 1})),
            _ => Ok(json!([])),
        }
    })
    .unwrap();
    assert_eq!(calls[0].1, "/zones?name=home.example.com&per_page=5");
    assert_eq!(calls[1].1, "/zones?name=example.com&per_page=5");
    assert_eq!(
        calls[2].1,
        format!("/zones/{ZONE}/dns_records?type=AAAA&name.exact={DOMAIN}&per_page=5")
    );
    assert_eq!(
        calls[3],
        (
            "POST".into(),
            format!("/zones/{ZONE}/dns_records"),
            Some(json!({
                "type": "AAAA", "name": DOMAIN, "content": ip.to_string(), "ttl": 1, "proxied": false
            }))
        )
    );
}

#[test]
fn patches_changed_address_or_ttl_and_skips_unchanged_or_ambiguous_records() {
    let ip = "2408:1234::1".parse().unwrap();
    for (content, ttl, count, expected, expect_error) in [
        (
            "2408:1234::2",
            1,
            1,
            Some(json!({"content": "2408:1234::1"})),
            false,
        ),
        (
            "2408:1234:0:0:0:0:0:1",
            60,
            1,
            Some(json!({"ttl": 1})),
            false,
        ),
        (
            "2408:1234::2",
            60,
            1,
            Some(json!({"content": "2408:1234::1", "ttl": 1})),
            false,
        ),
        ("2408:1234:0:0:0:0:0:1", 1, 1, None, false),
        ("2408:1234::2", 60, 2, None, true),
    ] {
        let mut patch = None;
        let result = cloudflare::update(DOMAIN, ip, |method, path, body| {
            if path.starts_with("/zones?name=") {
                let name = path.split_once('=').unwrap().1.split('&').next().unwrap();
                return Ok(json!([{ "name": name, "id": ZONE }]));
            }
            if method == "PATCH" {
                assert_eq!(path, format!("/zones/{ZONE}/dns_records/{RECORD}"));
                patch = body;
                return Ok(json!({"ttl": 1}));
            }
            assert_eq!(method, "GET");
            Ok(json!(vec![
                json!({"type": "AAAA", "id": RECORD, "name": DOMAIN, "content": content, "ttl": ttl});
                count
            ]))
        });
        assert_eq!(result.is_err(), expect_error);
        assert_eq!(patch, expected);
    }
}
