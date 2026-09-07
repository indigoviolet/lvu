//! Cache-pressure policy: reclaim disposable caches first, then stop loudly.
//!
//! The escalation is fixed and never skips a step:
//!
//! 1. reclaim disposable caches (row cache, query membership, derived indexes);
//! 2. apply backpressure when reclaiming everything disposable is not enough;
//! 3. stop acquisition with a visible error when it still is not enough.
//!
//! Captured records are never evicted to make room. A capture that stops
//! because storage ran out is reported as stopped, not presented as complete.
//! Command-enrichment output is classified durable: an arbitrary command is not
//! guaranteed reproducible, so its results are retained derived data.

use super::ledger::format_bytes;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Durability {
    /// Recomputable from durable data alone; safe to delete under pressure.
    Disposable,
    /// Must survive; only an explicit, recorded deletion may remove it.
    Durable,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CacheClass {
    RowCache,
    QueryMembership,
    DerivedIndex,
    RawCapture,
    CommandEnrichment,
    InvestigationExport,
    Workspace,
}

impl CacheClass {
    pub fn durability(self) -> Durability {
        match self {
            Self::RowCache | Self::QueryMembership | Self::DerivedIndex => Durability::Disposable,
            Self::RawCapture
            | Self::CommandEnrichment
            | Self::InvestigationExport
            | Self::Workspace => Durability::Durable,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::RowCache => "row cache (memory)",
            Self::QueryMembership => "query membership (memory)",
            Self::DerivedIndex => "derived indexes (disk)",
            Self::RawCapture => "captured records (disk)",
            Self::CommandEnrichment => "command results (disk)",
            Self::InvestigationExport => "investigations (disk)",
            Self::Workspace => "workspace and recipes (disk)",
        }
    }

    pub fn note(self) -> &'static str {
        match self {
            Self::RowCache => "rendered pages; rebuilt on demand",
            Self::QueryMembership => "matched identities held in memory for the open views",
            Self::DerivedIndex => "row indexes; recomputed from the journal",
            Self::RawCapture => "original captured bytes; deleted only when you say so",
            Self::CommandEnrichment => {
                "external command output; not assumed reproducible, so it is retained"
            }
            Self::InvestigationExport => "frozen datasets and manifests; pin the captures they use",
            Self::Workspace => "views, drafts, recipes and navigation state",
        }
    }
}

/// One row of the usage table, in the class the user should reason about.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheUsage {
    pub class: CacheClass,
    pub label: String,
    pub bytes: u64,
    pub limit: Option<u64>,
    /// Only ever non-zero for disposable classes.
    pub reclaimable_bytes: u64,
    pub durability: Durability,
    pub note: String,
}

impl CacheUsage {
    pub fn new(class: CacheClass, bytes: u64, limit: Option<u64>, reclaimable: u64) -> Self {
        let durability = class.durability();
        Self {
            class,
            label: class.label().into(),
            bytes,
            limit,
            reclaimable_bytes: match durability {
                Durability::Disposable => reclaimable,
                // A durable class has no reclaimable bytes by definition. This
                // is the guard that keeps a durable total out of a "space you
                // can free" figure.
                Durability::Durable => 0,
            },
            durability,
            note: class.note().into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DiskSpace {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Clone, Debug, Default)]
pub struct PressureInputs {
    pub disk: Option<DiskSpace>,
    /// Headroom kept free before acquisition is refused.
    pub reserve_bytes: u64,
    pub derived_index_bytes: u64,
    pub derived_index_limit: u64,
    pub row_cache_bytes: u64,
    pub row_cache_limit: u64,
    pub membership_bytes: u64,
    pub membership_limit: u64,
    /// Disposable disk bytes that are verified unused and can be removed now.
    pub reclaimable_disk_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PressureLevel {
    Normal,
    /// Disposable caches should be reclaimed.
    ReclaimCache,
    /// Reclaiming is not enough; acquisition must slow down.
    Backpressure,
    /// Nothing disposable is left; acquisition stops with a visible error.
    StopAcquisition,
}

#[derive(Clone, Debug)]
pub struct PressureDecision {
    pub level: PressureLevel,
    /// Classes to reclaim, in order. Never contains a durable class.
    pub reclaim: Vec<CacheClass>,
    /// Present exactly when acquisition must stop. The application shows this
    /// as a source acquisition error; a capture that stopped here is not a
    /// complete capture.
    pub acquisition_error: Option<String>,
    pub explanation: String,
    pub free_after_reclaim_bytes: Option<u64>,
}

impl Default for PressureDecision {
    fn default() -> Self {
        Self {
            level: PressureLevel::Normal,
            reclaim: Vec::new(),
            acquisition_error: None,
            explanation: "Storage is within its configured budgets.".into(),
            free_after_reclaim_bytes: None,
        }
    }
}

pub fn assess(inputs: &PressureInputs) -> PressureDecision {
    let mut reclaim = Vec::new();
    if inputs.derived_index_limit > 0 && inputs.derived_index_bytes > inputs.derived_index_limit {
        reclaim.push(CacheClass::DerivedIndex);
    }
    if inputs.row_cache_limit > 0 && inputs.row_cache_bytes > inputs.row_cache_limit {
        reclaim.push(CacheClass::RowCache);
    }
    if inputs.membership_limit > 0 && inputs.membership_bytes > inputs.membership_limit {
        reclaim.push(CacheClass::QueryMembership);
    }

    let Some(disk) = inputs.disk else {
        let level = if reclaim.is_empty() {
            PressureLevel::Normal
        } else {
            PressureLevel::ReclaimCache
        };
        return PressureDecision {
            level,
            explanation: if reclaim.is_empty() {
                "Storage is within its configured budgets. Free disk space is unknown.".into()
            } else {
                "A configured cache budget is exceeded. Free disk space is unknown.".into()
            },
            reclaim,
            acquisition_error: None,
            free_after_reclaim_bytes: None,
        };
    };

    if disk.available_bytes >= inputs.reserve_bytes {
        let level = if reclaim.is_empty() {
            PressureLevel::Normal
        } else {
            PressureLevel::ReclaimCache
        };
        return PressureDecision {
            level,
            explanation: format!(
                "{} free, above the {} reserve.{}",
                format_bytes(disk.available_bytes),
                format_bytes(inputs.reserve_bytes),
                if reclaim.is_empty() {
                    ""
                } else {
                    " A configured cache budget is exceeded and will be reclaimed."
                }
            ),
            reclaim,
            acquisition_error: None,
            free_after_reclaim_bytes: Some(disk.available_bytes),
        };
    }

    // Below the reserve. Disposable disk caches go first, whatever their budget
    // says, because they can be rebuilt from data that must not be touched.
    if !reclaim.contains(&CacheClass::DerivedIndex) && inputs.reclaimable_disk_bytes > 0 {
        reclaim.insert(0, CacheClass::DerivedIndex);
    }
    let after = disk
        .available_bytes
        .saturating_add(inputs.reclaimable_disk_bytes);
    if after >= inputs.reserve_bytes {
        return PressureDecision {
            level: PressureLevel::ReclaimCache,
            explanation: format!(
                "Only {} free, below the {} reserve. Reclaiming {} of disposable cache restores it; no captured record is evicted.",
                format_bytes(disk.available_bytes),
                format_bytes(inputs.reserve_bytes),
                format_bytes(inputs.reclaimable_disk_bytes)
            ),
            reclaim,
            acquisition_error: None,
            free_after_reclaim_bytes: Some(after),
        };
    }
    if inputs.reclaimable_disk_bytes > 0 {
        return PressureDecision {
            level: PressureLevel::Backpressure,
            explanation: format!(
                "Only {} free, below the {} reserve. Reclaiming every disposable cache reaches {}, which is still short, so acquisition is slowed while space is recovered.",
                format_bytes(disk.available_bytes),
                format_bytes(inputs.reserve_bytes),
                format_bytes(after)
            ),
            reclaim,
            acquisition_error: None,
            free_after_reclaim_bytes: Some(after),
        };
    }
    let message = format!(
        "Storage is full: {} free against a {} reserve, and no disposable cache is left to reclaim. Acquisition is stopped. Records arriving now are not captured; delete captures or investigations, or lower the reserve, then restart the source.",
        format_bytes(disk.available_bytes),
        format_bytes(inputs.reserve_bytes)
    );
    PressureDecision {
        level: PressureLevel::StopAcquisition,
        reclaim,
        explanation: message.clone(),
        acquisition_error: Some(message),
        free_after_reclaim_bytes: Some(after),
    }
}

/// Free space for the filesystem holding `path`.
#[cfg(unix)]
pub fn disk_space(path: &std::path::Path) -> std::io::Result<DiskSpace> {
    use std::os::unix::ffi::OsStrExt;
    let mut bytes = path.as_os_str().as_bytes().to_vec();
    bytes.push(0);
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `bytes` is a NUL-terminated copy of `path` and `stats` is a valid
    // owned `statvfs` for the duration of the call.
    let result = unsafe { libc::statvfs(bytes.as_ptr().cast(), &mut stats) };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let block = if stats.f_frsize > 0 {
        stats.f_frsize as u64
    } else {
        stats.f_bsize as u64
    };
    Ok(DiskSpace {
        total_bytes: (stats.f_blocks as u64).saturating_mul(block),
        available_bytes: (stats.f_bavail as u64).saturating_mul(block),
    })
}

#[cfg(not(unix))]
pub fn disk_space(_path: &std::path::Path) -> std::io::Result<DiskSpace> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "free space reporting is not implemented on this platform",
    ))
}
