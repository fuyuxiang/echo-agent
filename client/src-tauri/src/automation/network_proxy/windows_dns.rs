//! Resolve through DNS configured on an active Windows network adapter.

use super::InterfaceResolution;
use std::{
    mem::{size_of, MaybeUninit},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    ptr,
    sync::Arc,
    time::Duration,
};
use tokio::{sync::Semaphore, task::JoinSet, time::timeout};
use windows_sys::Win32::{
    Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_SUCCESS},
    NetworkManagement::{
        Dns::{
            DnsFree, DnsFreeRecordList, DnsQueryEx, DnsSectionAnswer, DNS_QUERY_BYPASS_CACHE,
            DNS_QUERY_NO_MULTICAST, DNS_QUERY_REQUEST, DNS_QUERY_REQUEST_VERSION1,
            DNS_QUERY_RESULT, DNS_QUERY_RESULTS_VERSION1, DNS_QUERY_TREAT_AS_FQDN, DNS_RECORDA,
            DNS_TYPE_A, DNS_TYPE_AAAA,
        },
        IpHelper::{
            GetAdaptersAddresses, GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST,
            GAA_FLAG_SKIP_FRIENDLY_NAME, GAA_FLAG_SKIP_MULTICAST, GAA_FLAG_SKIP_UNICAST,
            IF_TYPE_SOFTWARE_LOOPBACK, IP_ADAPTER_ADDRESSES_LH,
        },
        Ndis::IfOperStatusUp,
    },
    Networking::WinSock::AF_UNSPEC,
};

const RECORD_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_INTERFACES: usize = 4;
static QUERY_SLOTS: Semaphore = Semaphore::const_new(24);

pub(super) async fn lookup_on_interface(host: &str) -> Option<InterfaceResolution> {
    let interfaces = tokio::task::spawn_blocking(interfaces_with_dns)
        .await
        .ok()?;
    let name: Arc<[u16]> = host.encode_utf16().chain([0]).collect();
    let mut queries = JoinSet::new();
    for index in interfaces {
        let name = Arc::clone(&name);
        queries.spawn(async move {
            let (mut addresses, ipv6) = tokio::join!(
                query_records(index, Arc::clone(&name), DNS_TYPE_A),
                query_records(index, name, DNS_TYPE_AAAA),
            );
            addresses.extend(ipv6);
            InterfaceResolution {
                interface: index.to_string(),
                addresses,
            }
        });
    }
    // An unresponsive VPN adapter must not hold up the other configured adapters.
    while let Some(result) = queries.join_next().await {
        if let Ok(resolution) = result {
            if !resolution.addresses.is_empty() {
                return Some(resolution);
            }
        }
    }
    None
}

fn interfaces_with_dns() -> Vec<u32> {
    let mut bytes = 15 * 1024;
    for _ in 0..3 {
        // Use the adapter type to provide the alignment required by IP Helper.
        let slots = (bytes as usize).div_ceil(size_of::<IP_ADAPTER_ADDRESSES_LH>());
        let mut buffer = vec![MaybeUninit::<IP_ADAPTER_ADDRESSES_LH>::uninit(); slots];
        let head = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        let status = unsafe {
            GetAdaptersAddresses(
                u32::from(AF_UNSPEC),
                GAA_FLAG_INCLUDE_GATEWAYS
                    | GAA_FLAG_SKIP_UNICAST
                    | GAA_FLAG_SKIP_ANYCAST
                    | GAA_FLAG_SKIP_MULTICAST
                    | GAA_FLAG_SKIP_FRIENDLY_NAME,
                ptr::null(),
                head,
                &mut bytes,
            )
        };
        match status {
            ERROR_BUFFER_OVERFLOW => continue,
            // The linked list returned by IP Helper lives inside buffer.
            ERROR_SUCCESS => return unsafe { select_interfaces(head) },
            _ => break,
        }
    }
    Vec::new()
}

unsafe fn select_interfaces(mut next: *const IP_ADAPTER_ADDRESSES_LH) -> Vec<u32> {
    let mut gateways = Vec::new();
    let mut others = Vec::new();
    while let Some(interface) = unsafe { next.as_ref() } {
        next = interface.Next;
        if interface.OperStatus != IfOperStatusUp
            || interface.IfType == IF_TYPE_SOFTWARE_LOOPBACK
            || interface.FirstDnsServerAddress.is_null()
        {
            continue;
        }
        let ipv4_index = unsafe { interface.Anonymous1.Anonymous.IfIndex };
        let index = if ipv4_index == 0 {
            interface.Ipv6IfIndex
        } else {
            ipv4_index
        };
        let candidates = if interface.FirstGatewayAddress.is_null() {
            &mut others
        } else {
            &mut gateways
        };
        if index != 0 && candidates.len() < MAX_INTERFACES && !candidates.contains(&index) {
            candidates.push(index);
        }
    }
    // IP Helper returns Windows route order. Prefer adapters serving a default
    // gateway, with a fallback for links with on-link routes and no gateway.
    if gateways.is_empty() {
        others
    } else {
        gateways
    }
}

async fn query_records(index: u32, name: Arc<[u16]>, record_type: u16) -> Vec<IpAddr> {
    timeout(RECORD_TIMEOUT, async move {
        let Ok(slot) = QUERY_SLOTS.acquire().await else {
            return Vec::new();
        };
        tokio::task::spawn_blocking(move || {
            // The native call can outlive the async timeout. Keep the permit
            // until Windows returns so stalled lookups cannot grow unbounded.
            let _slot = slot;
            query(index, &name, record_type)
        })
        .await
        .unwrap_or_default()
    })
    .await
    .unwrap_or_default()
}

struct Records(*mut DNS_RECORDA);

impl Drop for Records {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { DnsFree(self.0.cast(), DnsFreeRecordList) };
        }
    }
}

fn query(index: u32, name: &[u16], record_type: u16) -> Vec<IpAddr> {
    let request = DNS_QUERY_REQUEST {
        Version: DNS_QUERY_REQUEST_VERSION1,
        QueryName: name.as_ptr(),
        QueryType: record_type,
        QueryOptions: u64::from(
            DNS_QUERY_TREAT_AS_FQDN | DNS_QUERY_BYPASS_CACHE | DNS_QUERY_NO_MULTICAST,
        ),
        InterfaceIndex: index,
        ..Default::default()
    };
    let mut result = DNS_QUERY_RESULT {
        Version: DNS_QUERY_RESULTS_VERSION1,
        ..Default::default()
    };
    // With no completion callback DnsQueryEx is synchronous, so the request
    // and its UTF-16 name remain valid throughout the call.
    let status = unsafe { DnsQueryEx(&request, &mut result, ptr::null_mut()) };
    let records = Records(result.pQueryRecords);
    let mut addresses = Vec::new();
    if status == 0 && result.QueryStatus == 0 {
        let mut next = records.0;
        while let Some(record) = unsafe { next.as_ref() } {
            next = record.pNext;
            // Section occupies the low two bits of the native record flags.
            if unsafe { record.Flags.DW } & 3 != DnsSectionAnswer as u32 {
                continue;
            }
            let ip = unsafe {
                match (record.wType, record.wDataLength) {
                    (DNS_TYPE_A, 4) => {
                        IpAddr::V4(Ipv4Addr::from(record.Data.A.IpAddress.to_ne_bytes()))
                    }
                    (DNS_TYPE_AAAA, 16) => {
                        IpAddr::V6(Ipv6Addr::from(record.Data.AAAA.Ip6Address.IP6Byte))
                    }
                    _ => continue,
                }
            };
            addresses.push(ip);
        }
    }
    addresses
}
