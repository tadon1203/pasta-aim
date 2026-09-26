use std::{
    alloc::{Layout, alloc, dealloc},
    ffi::CStr,
    mem::{align_of, size_of},
    os::raw::c_char,
    ptr::NonNull,
};

use anyhow::Error;
use windows::{
    Win32::Graphics::Direct3D12::{
        D3D12_DRED_ALLOCATION_NODE, ID3D12Device, ID3D12DeviceRemovedExtendedData,
    },
    core::Interface,
};

use super::{
    device::GpuDeviceContext,
    frame_mailbox::{FramePublisher, GpuFrameMetadata, SlotState, TransportSnapshot},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuFaultStage {
    CaptureCopy,
    CaptureReadySignal,
    D3D12ReadyWait,
    PreprocessSubmit,
    PreprocessCollect,
    BackendRun,
    ReuseSignal,
    DeviceRemoved,
    #[allow(dead_code)]
    Teardown,
}

use windows::Win32::Graphics::Direct3D::WKPDID_D3DDebugObjectName;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device4, ID3D11DeviceChild};
use windows::Win32::Graphics::Direct3D12::{D3D12_MESSAGE, ID3D12InfoQueue, ID3D12Object};
use windows::core::PCWSTR;

impl GpuFaultStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CaptureCopy => "capture_copy",
            Self::CaptureReadySignal => "capture_ready_signal",
            Self::D3D12ReadyWait => "d3d12_ready_wait",
            Self::PreprocessSubmit => "preprocess_submit",
            Self::PreprocessCollect => "preprocess_collect",
            Self::BackendRun => "backend_run",
            Self::ReuseSignal => "reuse_signal",
            Self::DeviceRemoved => "device_removed",
            Self::Teardown => "teardown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuDropReason {
    SupersededBeforeClaim,
    NoFreeSlot,
    StaleGeneration,
    ReuseNotComplete,
    PipelinePoisoned,
    ResultSuperseded,
}

impl GpuDropReason {
    pub const COUNT: usize = 6;

    pub const fn index(self) -> usize {
        match self {
            Self::SupersededBeforeClaim => 0,
            Self::NoFreeSlot => 1,
            Self::StaleGeneration => 2,
            Self::ReuseNotComplete => 3,
            Self::PipelinePoisoned => 4,
            Self::ResultSuperseded => 5,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GpuDropCounts(pub [u64; GpuDropReason::COUNT]);

impl GpuDropCounts {
    pub fn increment(&mut self, reason: GpuDropReason) {
        let value = &mut self.0[reason.index()];
        *value = value.saturating_add(1);
    }

    pub fn get(self, reason: GpuDropReason) -> u64 {
        self.0[reason.index()]
    }

    pub fn add_assign(&mut self, other: Self) {
        for (value, addition) in self.0.iter_mut().zip(other.0) {
            *value = value.saturating_add(addition);
        }
    }
}

#[derive(Debug, Clone)]
pub struct GpuFaultReport {
    pub pipeline_id: u64,
    pub prepare_id: String,
    pub generation: Option<u64>,
    pub stage: GpuFaultStage,
    pub slot_id: Option<usize>,
    pub frame_seq: Option<u64>,
    pub slot_state: Option<SlotState>,
    pub ready_value: Option<u64>,
    pub ready_completed: Option<u64>,
    pub reuse_value: Option<u64>,
    pub reuse_completed: Option<u64>,
    pub hr: Option<i32>,
    pub d3d11_removed_reason: Option<i32>,
    pub d3d12_removed_reason: Option<i32>,
    pub error: String,
    pub transport: Option<TransportSnapshot>,
    pub dred: Option<String>,
}

impl GpuFaultReport {
    pub fn formatted_fields(&self) -> String {
        format!(
            "pipeline_id={} prepare_id={} generation={} stage={} slot_id={} frame_seq={} slot_state={} ready_value={} ready_completed={} reuse_value={} reuse_completed={} hr={} d3d11_removed_reason={} d3d12_removed_reason={}",
            self.pipeline_id,
            self.prepare_id,
            optional(self.generation),
            self.stage.as_str(),
            optional(self.slot_id),
            optional(self.frame_seq),
            self.slot_state
                .map(|state| format!("{state:?}"))
                .unwrap_or_else(|| "unknown".to_owned()),
            optional(self.ready_value),
            optional(self.ready_completed),
            optional(self.reuse_value),
            optional(self.reuse_completed),
            hex(self.hr),
            hex(self.d3d11_removed_reason),
            hex(self.d3d12_removed_reason),
        )
    }

    pub fn log(&self) {
        let fields = self.formatted_fields();
        tracing::error!(
            event = "gpu_transport_fault",
            pipeline_id = self.pipeline_id,
            prepare_id = %self.prepare_id,
            generation = %optional(self.generation),
            stage = self.stage.as_str(),
            slot_id = %optional(self.slot_id),
            frame_seq = %optional(self.frame_seq),
            slot_state = ?self.slot_state,
            ready_value = %optional(self.ready_value),
            ready_completed = %optional(self.ready_completed),
            reuse_value = %optional(self.reuse_value),
            reuse_completed = %optional(self.reuse_completed),
            hr = %hex(self.hr),
            d3d11_removed_reason = %hex(self.d3d11_removed_reason),
            d3d12_removed_reason = %hex(self.d3d12_removed_reason),
            fields = %fields,
            error = %self.error,
            "gpu_transport_fault"
        );
        if let Some(dred) = &self.dred {
            tracing::error!(
                pipeline_id = self.pipeline_id,
                prepare_id = %self.prepare_id,
                report = %dred,
                "gpu_dred_report"
            );
        }
        if let Some(snapshot) = &self.transport {
            tracing::error!(
                pipeline_id = self.pipeline_id,
                prepare_id = %self.prepare_id,
                poisoned = snapshot.poisoned,
                drop_counts = ?snapshot.drop_counts,
                slots = ?snapshot.slots,
                "gpu_transport_flight_recorder"
            );
            for slot in &snapshot.slots {
                tracing::error!(
                    pipeline_id = self.pipeline_id,
                    prepare_id = %self.prepare_id,
                    slot_id = slot.slot_id,
                    state = ?slot.state,
                    frame_seq = ?slot.frame_seq,
                    ready_value = ?slot.ready_value,
                    reuse_value = ?slot.reuse_value,
                    "gpu_transport_slot"
                );
            }
        }
    }
}

/// Fence values observed when a fault was detected.
#[derive(Debug, Clone, Copy, Default)]
pub struct FenceProgress {
    pub reuse_value: Option<u64>,
    pub ready_completed: Option<u64>,
    pub reuse_completed: Option<u64>,
}

pub fn collect_gpu_fault_report(
    gpu: &GpuDeviceContext,
    mailbox: &FramePublisher,
    metadata: GpuFrameMetadata,
    stage: GpuFaultStage,
    fences: FenceProgress,
    error: &Error,
) -> GpuFaultReport {
    let FenceProgress {
        reuse_value,
        ready_completed,
        reuse_completed,
    } = fences;
    log_d3d12_messages(gpu.device(), true);
    let snapshot = mailbox.diagnostic_snapshot();
    let removed = d3d12_device_removed_reason(gpu.device());
    GpuFaultReport {
        pipeline_id: metadata.pipeline_id,
        prepare_id: snapshot.prepare_id.clone(),
        generation: Some(metadata.generation),
        stage,
        slot_id: Some(metadata.slot_id),
        frame_seq: Some(metadata.frame_seq),
        slot_state: snapshot.slots.get(metadata.slot_id).map(|slot| slot.state),
        ready_value: Some(metadata.capture_ready_value),
        ready_completed,
        reuse_value,
        reuse_completed,
        hr: error
            .downcast_ref::<windows::core::Error>()
            .map(|error| error.code().0),
        d3d11_removed_reason: None,
        d3d12_removed_reason: removed,
        error: format!("{error:#}"),
        transport: Some(snapshot),
        dred: collect_dred(gpu.device()),
    }
}

pub fn d3d11_device_removed_reason(device: &ID3D11Device4) -> Option<i32> {
    unsafe { device.GetDeviceRemovedReason() }
        .err()
        .map(|error| error.code().0)
}

pub fn d3d11_device_lost(device: &ID3D11Device4) -> bool {
    d3d11_device_removed_reason(device).is_some()
}

pub fn d3d12_device_removed_reason(device: &ID3D12Device) -> Option<i32> {
    unsafe { device.GetDeviceRemovedReason() }
        .err()
        .map(|error| error.code().0)
}

pub fn d3d12_device_lost(device: &ID3D12Device) -> bool {
    d3d12_device_removed_reason(device).is_some()
}

/// Prefix carried by the debug name of every D3D11/D3D12 object this application creates.
///
/// A D3D12 device is shared per adapter within the process, so the GUI's wgpu
/// swapchain traffic lands in the same `ID3D12InfoQueue` as ours. The prefix is
/// what lets diagnostics attribute a message to the inference pipeline.
pub const OWN_OBJECT_PREFIX: &str = "aimassist:";

pub fn name_d3d12<T: Interface>(object: &T, name: &str) {
    if let Ok(object) = object.cast::<ID3D12Object>() {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let _ = unsafe { object.SetName(PCWSTR(wide.as_ptr())) };
    }
}

pub fn name_d3d11<T: Interface>(object: &T, name: &str) {
    if let Ok(object) = object.cast::<ID3D11DeviceChild>() {
        let _ = unsafe {
            object.SetPrivateData(
                &WKPDID_D3DDebugObjectName,
                name.len() as u32,
                Some(name.as_ptr().cast()),
            )
        };
    }
}

/// Drains the D3D12 info queue. Messages naming our objects are errors; the rest
/// belong to other users of the shared device (e.g. the GUI's swapchain) and are
/// reported once per distinct message, at warn level, with an occurrence count.
pub fn log_d3d12_messages(device: &ID3D12Device, failed: bool) {
    let Ok(queue) = device.cast::<ID3D12InfoQueue>() else {
        return;
    };
    let count = unsafe { queue.GetNumStoredMessages() };
    let mut foreign: Vec<(String, u64)> = Vec::new();
    for index in 0..count {
        let mut message_len = 0usize;
        let _ = unsafe { queue.GetMessage(index, None, &mut message_len) };
        if message_len == 0 {
            continue;
        }
        let Ok(layout) = Layout::from_size_align(message_len, align_of::<D3D12_MESSAGE>()) else {
            continue;
        };
        let Some(buffer) = NonNull::new(unsafe { alloc(layout) }) else {
            continue;
        };
        let mut actual_len = message_len;
        let fetched =
            unsafe { queue.GetMessage(index, Some(buffer.as_ptr().cast()), &mut actual_len) };
        if fetched.is_ok() && actual_len >= size_of::<D3D12_MESSAGE>() {
            let message = unsafe { &*buffer.as_ptr().cast::<D3D12_MESSAGE>() };
            let description = if message.pDescription.is_null()
                || message.DescriptionByteLength == 0
            {
                String::new()
            } else {
                let bytes = unsafe {
                    std::slice::from_raw_parts(message.pDescription, message.DescriptionByteLength)
                };
                String::from_utf8_lossy(bytes)
                    .trim_end_matches(' ')
                    .to_owned()
            };
            if description.contains(OWN_OBJECT_PREFIX) {
                if failed {
                    tracing::error!(severity = ?message.Severity, category = ?message.Category, id = ?message.ID, description = %description, "d3d12_debug_message");
                } else {
                    tracing::debug!(severity = ?message.Severity, category = ?message.Category, id = ?message.ID, description = %description, "d3d12_debug_message");
                }
            } else {
                let key = normalize_message(&description);
                match foreign.iter_mut().find(|(known, _)| *known == key) {
                    Some((_, occurrences)) => *occurrences += 1,
                    None => foreign.push((key, 1)),
                }
            }
        }
        unsafe { dealloc(buffer.as_ptr(), layout) };
    }
    unsafe { queue.ClearStoredMessages() };
    for (description, occurrences) in foreign {
        tracing::warn!(occurrences, description = %description, "d3d12_debug_message_unattributed");
    }
}

/// Removes object addresses so repeated messages about different instances collapse.
fn normalize_message(description: &str) -> String {
    let mut normalized = String::with_capacity(description.len());
    let mut chars = description.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '0' && chars.peek() == Some(&'x') {
            chars.next();
            while chars.peek().is_some_and(|next| next.is_ascii_hexdigit()) {
                chars.next();
            }
            normalized.push_str("0x*");
        } else {
            normalized.push(ch);
        }
    }
    normalized
}

fn collect_dred(device: &ID3D12Device) -> Option<String> {
    let dred: ID3D12DeviceRemovedExtendedData = device.cast().ok()?;
    let mut lines = Vec::new();
    if let Ok(output) = unsafe { dred.GetAutoBreadcrumbsOutput() } {
        let mut node = output.pHeadAutoBreadcrumbNode;
        for _ in 0..32 {
            if node.is_null() {
                break;
            }
            let current = unsafe { &*node };
            let name = unsafe { dred_c_string(current.pCommandListDebugNameA) };
            let queue = unsafe { dred_c_string(current.pCommandQueueDebugNameA) };
            let completed = if current.pLastBreadcrumbValue.is_null() {
                None
            } else {
                Some(unsafe { *current.pLastBreadcrumbValue })
            };
            let operations = if current.pCommandHistory.is_null() || current.BreadcrumbCount == 0 {
                String::new()
            } else {
                let count = current.BreadcrumbCount.min(256) as usize;
                let history = unsafe { std::slice::from_raw_parts(current.pCommandHistory, count) };
                history[history.len().saturating_sub(8)..]
                    .iter()
                    .map(|operation| format!("{operation:?}"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            lines.push(format!("breadcrumb command_list={name:?} queue={queue:?} count={} completed={completed:?} recent_ops=[{operations}]", current.BreadcrumbCount));
            node = current.pNext;
        }
    }
    if let Ok(output) = unsafe { dred.GetPageFaultAllocationOutput() } {
        lines.push(format!("page_fault_va=0x{:016X}", output.PageFaultVA));
        append_allocation_names(&mut lines, output.pHeadExistingAllocationNode, "live");
        append_allocation_names(
            &mut lines,
            output.pHeadRecentFreedAllocationNode,
            "recently_freed",
        );
    }
    Some(if lines.is_empty() {
        "DRED data is not currently available".to_owned()
    } else {
        lines.join("; ")
    })
}

pub fn log_dred_report(device: &ID3D12Device) {
    if let Some(report) = collect_dred(device) {
        tracing::error!(report = %report, "gpu_dred_report");
    }
}

unsafe fn dred_c_string(value: *const u8) -> String {
    if value.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(value.cast::<c_char>()) }
        .to_string_lossy()
        .into_owned()
}

fn append_allocation_names(
    output: &mut Vec<String>,
    mut node: *const D3D12_DRED_ALLOCATION_NODE,
    label: &str,
) {
    for _ in 0..32 {
        if node.is_null() {
            break;
        }
        let current = unsafe { &*node };
        let name = unsafe { dred_c_string(current.ObjectNameA) };
        output.push(format!(
            "allocation {label}={name:?} type={:?}",
            current.AllocationType
        ));
        node = current.pNext;
    }
}

fn optional<T: std::fmt::Display>(value: Option<T>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_owned())
}

pub(crate) fn hex(value: Option<i32>) -> String {
    value
        .map(|value| format!("0x{:08X}", value as u32))
        .unwrap_or_else(|| "0x00000000".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_message_collapses_object_addresses() {
        let a =
            normalize_message("List (0x0000023240CFABF0:'x') resource (0x0000023240D94B90:'y')");
        let b =
            normalize_message("List (0x0000023240D2F900:'x') resource (0x0000023240D94B91:'y')");
        assert_eq!(a, b);
        assert!(a.contains("0x*"));
    }

    #[test]
    fn fault_event_fields_include_the_transport_identity_and_fences() {
        let report = GpuFaultReport {
            pipeline_id: 4,
            prepare_id: "12084-2".to_owned(),
            generation: Some(7),
            stage: GpuFaultStage::CaptureReadySignal,
            slot_id: Some(0),
            frame_seq: Some(1842),
            slot_state: Some(SlotState::CaptureWriting),
            ready_value: Some(1),
            ready_completed: Some(0),
            reuse_value: Some(0),
            reuse_completed: Some(0),
            hr: Some(0x80004005_u32 as i32),
            d3d11_removed_reason: Some(0x887A0006_u32 as i32),
            d3d12_removed_reason: Some(0x887A0005_u32 as i32),
            error: "signal failed".to_owned(),
            transport: None,
            dred: None,
        };
        let fields = report.formatted_fields();
        for expected in [
            "pipeline_id=4",
            "prepare_id=12084-2",
            "generation=7",
            "stage=capture_ready_signal",
            "slot_id=0",
            "frame_seq=1842",
            "slot_state=CaptureWriting",
            "ready_value=1",
            "ready_completed=0",
            "reuse_value=0",
            "reuse_completed=0",
            "hr=0x80004005",
            "d3d11_removed_reason=0x887A0006",
            "d3d12_removed_reason=0x887A0005",
        ] {
            assert!(fields.contains(expected), "missing {expected} in {fields}");
        }
    }
}
