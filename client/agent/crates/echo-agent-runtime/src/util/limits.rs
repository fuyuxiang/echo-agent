//! OS resource ceilings read at startup, so EMFILE/EAGAIN/OOM crash reports
//! carry the limits that were in effect.

/// Read once, rendered for the diagnostic log and for telemetry.
pub(crate) struct ProcessLimits {
    nofile: Option<(Option<u64>, Option<u64>)>,
    nproc: Option<(Option<u64>, Option<u64>)>,
    available_parallelism: Option<u64>,
    cgroup: Option<serde_json::Value>,
}

impl ProcessLimits {
    pub(crate) fn read() -> Self {
        Self {
            nofile: rlimit(RlimitKind::Nofile),
            nproc: rlimit(RlimitKind::Nproc),
            available_parallelism: std::thread::available_parallelism()
                .map(|n| usize::from(n) as u64)
                .ok(),
            cgroup: cgroup_v2_limits(),
        }
    }

    pub(crate) fn log(&self) {
        echo_agent_telemetry::unified_log::info(
            "process resource limits",
            None,
            Some(self.to_json()),
        );
    }

    fn to_json(&self) -> serde_json::Value {
        let pair = |v: Option<(Option<u64>, Option<u64>)>| {
            v.map(|(soft, hard)| serde_json::json!([soft, hard]))
        };
        serde_json::json!({
            "nofile": pair(self.nofile),
            "nproc": pair(self.nproc),
            "available_parallelism": self.available_parallelism,
            "cgroup": self.cgroup,
        })
    }

    fn cgroup_field(&self, name: &str) -> Option<String> {
        Some(self.cgroup.as_ref()?.get(name)?.as_str()?.to_owned())
    }

    pub(crate) fn into_event(self) -> echo_agent_telemetry::events::ProcessResourceLimits {
        let (nofile_soft, nofile_hard) = self.nofile.unwrap_or_default();
        let (nproc_soft, nproc_hard) = self.nproc.unwrap_or_default();
        echo_agent_telemetry::events::ProcessResourceLimits {
            nofile_soft,
            nofile_hard,
            nproc_soft,
            nproc_hard,
            available_parallelism: self.available_parallelism,
            cgroup_pids_max: self.cgroup_field("pids_max"),
            cgroup_memory_max: self.cgroup_field("memory_max"),
        }
    }
}

enum RlimitKind {
    Nofile,
    Nproc,
}

/// `(soft, hard)` for the given rlimit; `RLIM_INFINITY` maps to `None`.
#[cfg(unix)]
fn rlimit(kind: RlimitKind) -> Option<(Option<u64>, Option<u64>)> {
    let resource = match kind {
        RlimitKind::Nofile => libc::RLIMIT_NOFILE,
        RlimitKind::Nproc => libc::RLIMIT_NPROC,
    };
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes only into local `lim`.
    if unsafe { libc::getrlimit(resource, &mut lim) } != 0 {
        return None;
    }
    let val = |v: libc::rlim_t| (v != libc::RLIM_INFINITY).then_some(v);
    Some((val(lim.rlim_cur), val(lim.rlim_max)))
}

#[cfg(not(unix))]
fn rlimit(_kind: RlimitKind) -> Option<(Option<u64>, Option<u64>)> {
    None
}

/// Best-effort cgroup v2 pids/memory ceilings — the limits behind EAGAIN
/// thread-spawn failures and memcg OOM kills on shared hosts. `None` on any
/// read error or non-cgroup-v2 environment.
#[cfg(target_os = "linux")]
fn cgroup_v2_limits() -> Option<serde_json::Value> {
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    // cgroup v2 unified hierarchy line: "0::<path>".
    let path = cgroup.lines().find_map(|l| l.strip_prefix("0::"))?.trim();
    let read = |f: &str| {
        std::fs::read_to_string(format!("/sys/fs/cgroup{path}/{f}"))
            .ok()
            .map(|s| s.trim().to_owned())
    };
    Some(serde_json::json!({
        "pids_current": read("pids.current"),
        "pids_max": read("pids.max"),
        "memory_current": read("memory.current"),
        "memory_max": read("memory.max"),
    }))
}

#[cfg(not(target_os = "linux"))]
fn cgroup_v2_limits() -> Option<serde_json::Value> {
    None
}

#[cfg(test)]
mod tests {
    use super::ProcessLimits;

    #[test]
    #[cfg(unix)]
    fn the_log_shape_carries_rlimits_and_parallelism() {
        let v = ProcessLimits::read().to_json();
        assert!(v["nofile"].is_array(), "nofile missing: {v}");
        assert!(v["nproc"].is_array(), "nproc missing: {v}");
        assert!(v["available_parallelism"].is_u64(), "parallelism: {v}");
    }
}
