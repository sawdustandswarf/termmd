//! `zwp_linux_dmabuf_v1` (v4+): the compositor's device and format/modifier
//! feedback, and turning dma-bufs into `wl_buffer`s.

use std::os::fd::{AsFd, BorrowedFd};

use super::protocols::{wl_buffer, zwp_linux_buffer_params_v1, zwp_linux_dmabuf_feedback_v1, zwp_linux_dmabuf_v1};
use super::wire::Outgoing;
use crate::sys;

/// One preference tranche: formats usable on `device`, best first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tranche {
    /// `dev_t` of the target device.
    pub device: u64,
    /// The compositor could scan these out directly (no composition copy).
    pub scanout: bool,
    /// `(fourcc, modifier)` pairs.
    pub formats: Vec<(u32, u64)>,
}

/// What the compositor prefers, from the latest feedback `done`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DmabufFeedback {
    /// `dev_t` of the device the compositor composites with.
    pub main_device: u64,
    pub tranches: Vec<Tranche>,
}

impl DmabufFeedback {
    /// Modifiers for `fourcc` usable on the main device, in preference
    /// order without duplicates.
    pub fn modifiers(&self, fourcc: u32) -> Vec<u64> {
        let mut out = Vec::new();
        for t in self.tranches.iter().filter(|t| t.device == self.main_device) {
            for &(f, m) in &t.formats {
                if f == fourcc && !out.contains(&m) {
                    out.push(m);
                }
            }
        }
        out
    }
}

fn dev_t(bytes: &[u8]) -> u64 {
    bytes.get(..8).map_or(0, |b| u64::from_ne_bytes(b.try_into().expect("8 bytes")))
}

/// Accumulates feedback events until `done`.
#[derive(Debug, Default)]
pub(super) struct FeedbackBuilder {
    /// The format table: 16-byte entries of fourcc, padding, modifier.
    table: Vec<(u32, u64)>,
    pending: DmabufFeedback,
    tranche: Tranche,
}

impl FeedbackBuilder {
    /// Feed one event; returns the complete feedback on `done`.
    pub fn handle(&mut self, ev: zwp_linux_dmabuf_feedback_v1::Event) -> Option<DmabufFeedback> {
        use zwp_linux_dmabuf_feedback_v1::Event as E;
        match ev {
            E::FormatTable { fd, size } => {
                let bytes = sys::read_mapped(fd.as_fd(), size as usize).unwrap_or_default();
                self.table = parse_table(&bytes);
            }
            E::MainDevice { device } => self.pending.main_device = dev_t(&device),
            E::TrancheTargetDevice { device } => self.tranche.device = dev_t(&device),
            E::TrancheFlags { flags } => {
                self.tranche.scanout = flags.contains(zwp_linux_dmabuf_feedback_v1::TrancheFlags(1))
            }
            E::TrancheFormats { indices } => {
                for i in indices.as_chunks::<2>().0.iter().map(|&c| u16::from_ne_bytes(c) as usize) {
                    if let Some(&entry) = self.table.get(i) {
                        self.tranche.formats.push(entry);
                    }
                }
            }
            E::TrancheDone => self.pending.tranches.push(std::mem::take(&mut self.tranche)),
            E::Done => return Some(std::mem::take(&mut self.pending)),
        }
        None
    }
}

fn parse_table(bytes: &[u8]) -> Vec<(u32, u64)> {
    bytes
        .as_chunks::<16>()
        .0
        .iter()
        .map(|e| {
            let fourcc = u32::from_ne_bytes(e[0..4].try_into().expect("4 bytes"));
            let modifier = u64::from_ne_bytes(e[8..16].try_into().expect("8 bytes"));
            (fourcc, modifier)
        })
        .collect()
}

/// A single-plane dma-buf to wrap in a `wl_buffer`.
#[derive(Debug, Clone, Copy)]
pub struct BufferDesc<'a> {
    pub fd: BorrowedFd<'a>,
    pub width: u32,
    pub height: u32,
    pub fourcc: u32,
    pub modifier: u64,
    pub offset: u32,
    pub stride: u32,
}

/// Create a `wl_buffer` for `desc` (the compositor validates it later; a
/// bad buffer is a protocol error).
pub(super) fn create_buffer(
    dmabuf: zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
    desc: &BufferDesc<'_>,
    out: &mut Outgoing,
) -> std::io::Result<wl_buffer::WlBuffer> {
    let params = dmabuf.create_params(out);
    let m = desc.modifier;
    params.add(out, desc.fd, 0, desc.offset, desc.stride, (m >> 32) as u32, m as u32)?;
    let buffer = params.create_immed(
        out,
        desc.width as i32,
        desc.height as i32,
        desc.fourcc,
        zwp_linux_buffer_params_v1::Flags(0),
    );
    params.destroy(out);
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_tranches_and_modifiers() {
        let ar24 = 0x3432_5241u32;
        let xr24 = 0x3432_5258u32;
        let mut table = Vec::new();
        for (f, m) in [(ar24, 0u64), (ar24, 0x0100_0000_0000_0002), (xr24, 0), (ar24, 0x0100_0000_0000_0001)] {
            table.extend_from_slice(&f.to_ne_bytes());
            table.extend_from_slice(&[0; 4]);
            table.extend_from_slice(&m.to_ne_bytes());
        }
        let mut b = FeedbackBuilder { table: parse_table(&table), ..Default::default() };
        use zwp_linux_dmabuf_feedback_v1::{Event as E, TrancheFlags};
        let dev = |d: u64| d.to_ne_bytes().to_vec();
        let idx = |i: &[u16]| i.iter().flat_map(|x| x.to_ne_bytes()).collect::<Vec<u8>>();
        assert!(b.handle(E::MainDevice { device: dev(0xe280) }).is_none());
        // A scanout tranche preferring Y-tiled, then the general one.
        b.handle(E::TrancheTargetDevice { device: dev(0xe280) });
        b.handle(E::TrancheFlags { flags: TrancheFlags(1) });
        b.handle(E::TrancheFormats { indices: idx(&[1]) });
        b.handle(E::TrancheDone);
        b.handle(E::TrancheTargetDevice { device: dev(0xe280) });
        b.handle(E::TrancheFormats { indices: idx(&[0, 1, 2, 3, 99]) });
        b.handle(E::TrancheDone);
        // Another GPU's tranche is ignored for modifiers.
        b.handle(E::TrancheTargetDevice { device: dev(0xe281) });
        b.handle(E::TrancheFormats { indices: idx(&[0]) });
        b.handle(E::TrancheDone);
        let fb = b.handle(E::Done).expect("done");
        assert_eq!(fb.main_device, 0xe280);
        assert_eq!(fb.tranches.len(), 3);
        assert!(fb.tranches[0].scanout && !fb.tranches[1].scanout);
        assert_eq!(fb.modifiers(ar24), [0x0100_0000_0000_0002, 0, 0x0100_0000_0000_0001]);
        assert_eq!(fb.modifiers(xr24), [0]);
    }
}
