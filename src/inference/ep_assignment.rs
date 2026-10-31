//! The only module that crosses from `ort`'s safe session wrapper into the ORT C API.

use std::{collections::HashMap, ffi::CStr, slice};

use anyhow::{Context, Result, bail};
use ort::{AsPointer, session::Session, sys};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EpAssignedNode {
    pub name: String,
    pub domain: String,
    pub op_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EpProviderAssignment {
    /// The unmodified execution-provider name returned by ONNX Runtime.
    pub name: String,
    pub subgraph_count: usize,
    pub nodes: Vec<EpAssignedNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct EpAssignmentReport {
    pub providers: Vec<EpProviderAssignment>,
    pub total_nodes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EpAssignmentState {
    Available(EpAssignmentReport),
    Unavailable(String),
}

pub(crate) fn read(session: &Session) -> Result<EpAssignmentReport> {
    let inner = session.inner();
    let mut subgraphs = std::ptr::null();
    let mut subgraph_count = 0_usize;

    // ORT owns assignment records for the lifetime of the session. This function copies every
    // string before returning, so no raw pointer is retained by the application.
    unsafe {
        call((ort::api().Session_GetEpGraphAssignmentInfo)(
            inner.ptr(),
            &mut subgraphs,
            &mut subgraph_count,
        ))?;

        let subgraphs = raw_slice(subgraphs, subgraph_count, "EP subgraph list")?;
        let mut providers = Vec::<EpProviderAssignment>::new();
        let mut provider_indices = HashMap::<String, usize>::new();

        for &subgraph in subgraphs {
            if subgraph.is_null() {
                bail!("ORT returned a null EP subgraph");
            }
            let provider_name = get_subgraph_name(subgraph)?;
            let provider_index = if let Some(index) = provider_indices.get(&provider_name) {
                *index
            } else {
                let index = providers.len();
                provider_indices.insert(provider_name.clone(), index);
                providers.push(EpProviderAssignment {
                    name: provider_name,
                    subgraph_count: 0,
                    nodes: Vec::new(),
                });
                index
            };

            let provider = &mut providers[provider_index];
            provider.subgraph_count += 1;
            let mut nodes = std::ptr::null();
            let mut node_count = 0_usize;
            call((ort::api().EpAssignedSubgraph_GetNodes)(
                subgraph,
                &mut nodes,
                &mut node_count,
            ))?;
            for &node in raw_slice(nodes, node_count, "EP node list")? {
                if node.is_null() {
                    bail!("ORT returned a null EP-assigned node");
                }
                provider.nodes.push(EpAssignedNode {
                    name: get_node_string(node, ort::api().EpAssignedNode_GetName, "name")?,
                    domain: get_node_string(node, ort::api().EpAssignedNode_GetDomain, "domain")?,
                    op_type: get_node_string(
                        node,
                        ort::api().EpAssignedNode_GetOperatorType,
                        "operator type",
                    )?,
                });
            }
        }

        let total_nodes = providers.iter().map(|provider| provider.nodes.len()).sum();
        Ok(EpAssignmentReport {
            providers,
            total_nodes,
        })
    }
}

unsafe fn call(status: sys::OrtStatusPtr) -> Result<()> {
    unsafe { ort::Error::result_from_status(status) }.map_err(anyhow::Error::from)
}

unsafe fn raw_slice<'a, T>(ptr: *const T, len: usize, description: &str) -> Result<&'a [T]> {
    if len == 0 {
        return Ok(&[]);
    }
    if ptr.is_null() {
        bail!("ORT returned a null {description} with length {len}");
    }
    Ok(unsafe { slice::from_raw_parts(ptr, len) })
}

unsafe fn c_string(ptr: *const std::ffi::c_char, description: &str) -> Result<String> {
    if ptr.is_null() {
        bail!("ORT returned a null {description}");
    }
    Ok(unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .with_context(|| format!("ORT returned non-UTF-8 {description}"))?
        .to_owned())
}

unsafe fn get_subgraph_name(subgraph: *const sys::OrtEpAssignedSubgraph) -> Result<String> {
    let mut name = std::ptr::null();
    unsafe {
        call((ort::api().EpAssignedSubgraph_GetEpName)(
            subgraph, &mut name,
        ))?;
        c_string(name, "EP name")
    }
}

type NodeStringGetter = unsafe extern "system" fn(
    *const sys::OrtEpAssignedNode,
    *mut *const std::ffi::c_char,
) -> sys::OrtStatusPtr;

unsafe fn get_node_string(
    node: *const sys::OrtEpAssignedNode,
    getter: NodeStringGetter,
    description: &str,
) -> Result<String> {
    let mut value = std::ptr::null();
    unsafe {
        call(getter(node, &mut value))?;
        c_string(value, description)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_counts_nodes_across_providers() {
        let report = EpAssignmentReport {
            providers: vec![
                EpProviderAssignment {
                    name: "DmlExecutionProvider".into(),
                    subgraph_count: 2,
                    nodes: vec![EpAssignedNode {
                        name: "a".into(),
                        domain: String::new(),
                        op_type: "Conv".into(),
                    }],
                },
                EpProviderAssignment {
                    name: "CPUExecutionProvider".into(),
                    subgraph_count: 1,
                    nodes: vec![EpAssignedNode {
                        name: "b".into(),
                        domain: String::new(),
                        op_type: "Shape".into(),
                    }],
                },
            ],
            total_nodes: 2,
        };
        assert_eq!(
            report
                .providers
                .iter()
                .map(|provider| provider.nodes.len())
                .sum::<usize>(),
            report.total_nodes
        );
    }
}
