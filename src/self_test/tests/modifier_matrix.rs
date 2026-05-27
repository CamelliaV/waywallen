use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

use anyhow::{anyhow, Result};
use ash::vk;

use super::super::proto::{recv_msg, send_msg, TestMsg};
use super::super::report::{ModifierMatrix, ModifierResult, ProbeOutcome};
use super::super::vk::device::VkDevice;
use super::super::vk::image::{create_with_modifiers, export_dmabuf};
use super::super::vk::modifier::{format_modifier, query_supported, ModifierEntry};

const FORMAT: vk::Format = vk::Format::R8G8B8A8_UNORM;
const FOURCC_AB24: u32 = 0x34324241;
const WIDTH: u32 = 1024;
const HEIGHT: u32 = 1024;

pub fn run_orchestrator(
    instance: &ash::Instance,
    phys: vk::PhysicalDevice,
    vkd: &VkDevice,
    sock: &UnixStream,
) -> Result<ModifierMatrix> {
    let mut entries = query_supported(instance, phys, FORMAT)?;
    log::info!(
        "modifier_matrix: driver advertises {} modifier(s)",
        entries.len()
    );

    entries.sort_by_key(|e| (e.modifier == 0) as u8);
    let mut results: Vec<ModifierResult> = Vec::with_capacity(entries.len());

    for entry in &entries {
        log::info!(
            "modifier_matrix: probing {:#x} ({})",
            entry.modifier,
            format_modifier(entry.modifier)
        );
        let outcome = probe_one_modifier(vkd, sock, entry)?;
        results.push(outcome);
    }

    Ok(ModifierMatrix { modifiers: results })
}

fn probe_one_modifier(
    vkd: &VkDevice,
    sock: &UnixStream,
    entry: &ModifierEntry,
) -> Result<ModifierResult> {
    let img = match create_with_modifiers(
        vkd,
        WIDTH,
        HEIGHT,
        FORMAT,
        vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC,
        &[entry.modifier],
        false,
    ) {
        Ok(i) => i,
        Err(e) => {
            return Ok(ModifierResult {
                fourcc: FOURCC_AB24,
                modifier: entry.modifier,
                modifier_name: format_modifier(entry.modifier),
                producer: ProbeOutcome::Fail {
                    vk_result: 0,
                    message: format!("alloc: {e}"),
                },
                consumer: ProbeOutcome::Fail {
                    vk_result: 0,
                    message: "skipped — producer alloc failed".into(),
                },
            });
        }
    };

    let fd = match export_dmabuf(vkd, &img) {
        Ok(fd) => fd,
        Err(e) => {
            return Ok(ModifierResult {
                fourcc: FOURCC_AB24,
                modifier: img.modifier,
                modifier_name: format_modifier(img.modifier),
                producer: ProbeOutcome::Fail {
                    vk_result: 0,
                    message: format!("export: {e}"),
                },
                consumer: ProbeOutcome::Fail {
                    vk_result: 0,
                    message: "skipped — producer export failed".into(),
                },
            });
        }
    };

    send_msg(
        sock,
        &TestMsg::ProbeModifier {
            fourcc: FOURCC_AB24,
            modifier: img.modifier,
            width: img.width,
            height: img.height,
            plane_stride: u32::try_from(img.plane0_stride).unwrap_or(u32::MAX),
            plane_offset: u32::try_from(img.plane0_offset).unwrap_or(0),
            plane_size: img.plane0_size,
        },
        &[fd.as_raw_fd()],
    )
    .map_err(|e| anyhow!("send ProbeModifier: {e}"))?;
    drop(fd);

    let (msg, _fds) = recv_msg(sock).map_err(|e| anyhow!("recv ProbeResult: {e}"))?;
    match msg {
        TestMsg::ProbeResult {
            fourcc,
            modifier,
            ok,
            vk_result,
            message,
        } => {
            let consumer = if ok {
                ProbeOutcome::Ok
            } else {
                ProbeOutcome::Fail { vk_result, message }
            };
            Ok(ModifierResult {
                fourcc,
                modifier,
                modifier_name: format_modifier(modifier),
                producer: ProbeOutcome::Ok,
                consumer,
            })
        }
        other => Err(anyhow!("expected ProbeResult, got {other:?}")),
    }
}

pub fn run_peer(vkd: &VkDevice, sock: &UnixStream) -> Result<()> {
    loop {
        let (msg, fds) = recv_msg(sock).map_err(|e| anyhow!("peer recv: {e}"))?;
        match msg {
            TestMsg::ProbeModifier {
                fourcc,
                modifier,
                width,
                height,
                plane_stride,
                plane_offset,
                plane_size: _,
            } => {
                if fds.len() != 1 {
                    anyhow::bail!("ProbeModifier missing dma-buf fd");
                }
                let mut fds_iter = fds.into_iter();
                let fd = fds_iter.next().unwrap();
                let result = super::super::vk::image::import_dmabuf(
                    vkd,
                    fd,
                    width,
                    height,
                    FORMAT,
                    vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC,
                    modifier,
                    plane_stride as u64,
                    plane_offset as u64,
                );
                let reply = match result {
                    Ok(_img) => TestMsg::ProbeResult {
                        fourcc,
                        modifier,
                        ok: true,
                        vk_result: 0,
                        message: String::new(),
                    },
                    Err(e) => TestMsg::ProbeResult {
                        fourcc,
                        modifier,
                        ok: false,
                        vk_result: -1,
                        message: format!("{e}"),
                    },
                };
                send_msg(sock, &reply, &[]).map_err(|e| anyhow!("send ProbeResult: {e}"))?;
            }
            TestMsg::MatrixDone => {
                send_msg(sock, &TestMsg::Ack, &[]).map_err(|e| anyhow!("send Ack: {e}"))?;
                return Ok(());
            }
            other => anyhow::bail!("unexpected {other:?}"),
        }
    }
}
