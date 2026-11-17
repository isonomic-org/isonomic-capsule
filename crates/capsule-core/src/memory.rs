// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! How much memory a new process can have now, as the system itself counts
//! it, and whether the system is under memory pressure.
//!
//! On macOS free pages are always few: the system keeps recently used file
//! data in inactive pages and hands them out on demand. So what a model can
//! use is free plus inactive plus speculative pages, from the kernel's own
//! VM statistics. Purgeable pages are not added: they already sit in the
//! active or inactive queues, and counting them again would overstate. The
//! system's own pressure level says when even that is not true any more.
//!
//! Elsewhere the system's "available" figure already means what can be had
//! without swapping, and is used as it is.

/// The system's own view of memory pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pressure {
    Normal,
    /// Memory is tight; the system is compressing and reclaiming.
    Warning,
    /// The system is short of memory and is swapping or killing processes.
    Critical,
    /// Not reported on this system.
    Unknown,
}

/// The kernel's VM page counts (macOS `host_statistics64`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmPages {
    pub page_size: u64,
    pub free: u64,
    pub inactive: u64,
    pub speculative: u64,
    pub purgeable: u64,
}

impl VmPages {
    /// What a new process can be given without paging anything out.
    pub fn reclaimable_bytes(&self) -> u64 {
        let pages = self.free + self.inactive + self.speculative;
        pages.saturating_mul(self.page_size)
    }
}

/// macOS's `kern.memorystatus_vm_pressure_level`: 1 normal, 2 warning,
/// 4 critical.
pub fn pressure_from_level(level: i32) -> Pressure {
    match level {
        1 => Pressure::Normal,
        2 => Pressure::Warning,
        4 => Pressure::Critical,
        _ => Pressure::Unknown,
    }
}

/// This Mac's VM page counts and pressure level, read from the kernel.
#[cfg(target_os = "macos")]
pub fn read_macos() -> Option<(VmPages, Pressure)> {
    // SAFETY: each call writes only into the values passed to it, with the
    // sizes given; failures are reported by their return codes.
    unsafe {
        let mut stats: libc::vm_statistics64 = std::mem::zeroed();
        let mut count = libc::HOST_VM_INFO64_COUNT;
        #[allow(deprecated)]
        let host = libc::mach_host_self();
        let done = libc::host_statistics64(
            host,
            libc::HOST_VM_INFO64,
            &mut stats as *mut libc::vm_statistics64 as libc::host_info64_t,
            &mut count,
        );
        if done != libc::KERN_SUCCESS {
            return None;
        }
        let page_size = libc::sysconf(libc::_SC_PAGESIZE);
        if page_size <= 0 {
            return None;
        }
        let pages = VmPages {
            page_size: page_size as u64,
            free: u64::from(stats.free_count),
            inactive: u64::from(stats.inactive_count),
            speculative: u64::from(stats.speculative_count),
            purgeable: u64::from(stats.purgeable_count),
        };
        let mut level: libc::c_int = 0;
        let mut size = std::mem::size_of::<libc::c_int>();
        let name = c"kern.memorystatus_vm_pressure_level";
        let read = libc::sysctlbyname(
            name.as_ptr(),
            &mut level as *mut libc::c_int as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        );
        let pressure = match read {
            0 => pressure_from_level(level),
            _ => Pressure::Unknown,
        };
        Some((pages, pressure))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Intel Mac in ordinary use (a browser and a design tool open),
    /// where the gate wrongly refused a 1.3 GB model: `vm_stat` and
    /// `memory_pressure` at that moment.
    pub(crate) const BUSY_MAC: VmPages = VmPages {
        page_size: 4096,
        free: 41_094,
        inactive: 1_403_926,
        speculative: 1_253,
        purgeable: 122_082,
    };

    #[test]
    fn a_busy_mac_still_has_its_inactive_pages_to_give() {
        // About 160 MB free, but 5.9 GB a new process can have.
        assert_eq!(BUSY_MAC.free * BUSY_MAC.page_size, 168_321_024);
        assert_eq!(BUSY_MAC.reclaimable_bytes(), 5_923_934_208);
    }

    #[test]
    fn the_pressure_levels_are_the_kernels() {
        assert_eq!(pressure_from_level(1), Pressure::Normal);
        assert_eq!(pressure_from_level(2), Pressure::Warning);
        assert_eq!(pressure_from_level(4), Pressure::Critical);
        assert_eq!(pressure_from_level(0), Pressure::Unknown);
    }
}
