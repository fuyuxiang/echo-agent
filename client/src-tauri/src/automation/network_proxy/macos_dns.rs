//! Resolve through the DNS configuration of the current macOS primary interface.

use super::InterfaceResolution;
use core_foundation::{
    base::{CFType, CFTypeRef, TCFType},
    dictionary::CFDictionary,
    string::{CFString, CFStringRef},
};
use libc::{c_char, c_void, sockaddr};
use std::{
    ffi::CString,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    ptr,
    time::{Duration, Instant},
};

const SCOPED_LOOKUP_TIMEOUT: Duration = Duration::from_secs(4);
const MORE_COMING: u32 = 1;
const ADD_RESULT: u32 = 2;

type Service = *mut c_void;
type AddressCallback =
    unsafe extern "C" fn(Service, u32, u32, i32, *const c_char, *const sockaddr, u32, *mut c_void);

#[link(name = "SystemConfiguration", kind = "framework")]
unsafe extern "C" {
    fn SCDynamicStoreCopyValue(store: *const c_void, key: CFStringRef) -> CFTypeRef;
}

unsafe extern "C" {
    fn DNSServiceGetAddrInfo(
        service: *mut Service,
        flags: u32,
        interface: u32,
        protocol: u32,
        host: *const c_char,
        callback: AddressCallback,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceRefSockFD(service: Service) -> libc::c_int;
    fn DNSServiceProcessResult(service: Service) -> i32;
    fn DNSServiceRefDeallocate(service: Service);
}

struct Lookup(Service);

impl Drop for Lookup {
    fn drop(&mut self) {
        // The service remains exclusively owned by this blocking worker.
        unsafe { DNSServiceRefDeallocate(self.0) };
    }
}

#[derive(Default)]
struct Reply {
    addresses: Vec<IpAddr>,
    complete: bool,
}

pub(super) async fn lookup_on_interface(host: &str) -> Option<InterfaceResolution> {
    let query = host.to_owned();
    tokio::task::spawn_blocking(move || {
        let interface = primary_interface("State:/Network/Global/IPv4")
            .or_else(|| primary_interface("State:/Network/Global/IPv6"))?;
        let addresses = lookup(&query, &interface)?;
        Some(InterfaceResolution {
            interface,
            addresses,
        })
    })
    .await
    .ok()?
}

fn primary_interface(state_key: &str) -> Option<String> {
    let key = CFString::new(state_key);
    // NULL selects a temporary System Configuration session. A non-null
    // result is retained and is released by CFType.
    let value = unsafe { SCDynamicStoreCopyValue(ptr::null(), key.as_concrete_TypeRef()) };
    if value.is_null() {
        return None;
    }
    let value = unsafe { CFType::wrap_under_create_rule(value) };
    let config = value.downcast_into::<CFDictionary>()?;
    let key = CFString::new("PrimaryInterface");
    let name = *config.find(key.as_CFTypeRef())?;
    let name = unsafe { CFType::wrap_under_get_rule(name) };
    Some(name.downcast_into::<CFString>()?.to_string())
}

fn lookup(host: &str, interface: &str) -> Option<Vec<IpAddr>> {
    let host = CString::new(format!("{}.", host.trim_end_matches('.'))).ok()?;
    let interface = CString::new(interface).ok()?;
    let index = unsafe { libc::if_nametoindex(interface.as_ptr()) };
    if index == 0 {
        return None;
    }
    let mut reply = Reply::default();
    let mut service = ptr::null_mut();
    // The callback is driven synchronously by DNSServiceProcessResult on
    // this thread. The stack-owned reply outlives the service.
    let result = unsafe {
        DNSServiceGetAddrInfo(
            &mut service,
            0,
            index,
            0,
            host.as_ptr(),
            receive_address,
            ptr::from_mut(&mut reply).cast(),
        )
    };
    if result != 0 || service.is_null() {
        return None;
    }
    let service = Lookup(service);
    let fd = unsafe { DNSServiceRefSockFD(service.0) };
    if fd < 0 {
        return None;
    }
    let deadline = Instant::now() + SCOPED_LOOKUP_TIMEOUT;
    loop {
        if reply.complete {
            break;
        }
        let wait = deadline.checked_duration_since(Instant::now())?;
        let mut fd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut fd, 1, wait.as_millis() as i32) };
        if ready <= 0 || fd.revents & libc::POLLIN == 0 {
            return None;
        }
        if unsafe { DNSServiceProcessResult(service.0) } != 0 {
            return None;
        }
    }
    (!reply.addresses.is_empty()).then_some(reply.addresses)
}

unsafe extern "C" fn receive_address(
    _: Service,
    flags: u32,
    _: u32,
    error: i32,
    _: *const c_char,
    address: *const sockaddr,
    _: u32,
    context: *mut c_void,
) {
    // DNS-SD supplies a sockaddr of the indicated family for successful
    // callbacks. The context is the worker's Reply during ProcessResult.
    let reply = unsafe { &mut *context.cast::<Reply>() };
    if error != 0 {
        reply.complete = true;
        return;
    }
    reply.complete = flags & MORE_COMING == 0;
    if address.is_null() || flags & ADD_RESULT == 0 {
        return;
    }
    let ip = unsafe {
        match i32::from((*address).sa_family) {
            libc::AF_INET => {
                let address = &*address.cast::<libc::sockaddr_in>();
                IpAddr::V4(Ipv4Addr::from(address.sin_addr.s_addr.to_ne_bytes()))
            }
            libc::AF_INET6 => {
                let address = &*address.cast::<libc::sockaddr_in6>();
                IpAddr::V6(Ipv6Addr::from(address.sin6_addr.s6_addr))
            }
            _ => return,
        }
    };
    reply.addresses.push(ip);
}
