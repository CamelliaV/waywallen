use std::io::{IoSlice, IoSliceMut, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

use nix::sys::socket::{recvmsg, sendmsg, ControlMessage, ControlMessageOwned, MsgFlags};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

fn default_true() -> bool {
    true
}

const MAX_FDS: usize = 8;
const MAX_BODY_BYTES: usize = u16::MAX as usize - 4;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TestMsg {
    Hello {
        version: u32,
        device_uuid_hex: String,
        driver_uuid_hex: String,
        device_name: String,
    },
    Welcome {
        ok: bool,
        message: String,
    },

    ProbeModifier {
        fourcc: u32,
        modifier: u64,
        width: u32,
        height: u32,
        plane_stride: u32,
        plane_offset: u32,
        plane_size: u64,
    },
    ProbeResult {
        fourcc: u32,
        modifier: u64,
        ok: bool,
        vk_result: i32,
        message: String,
    },
    MatrixDone,

    BindPair {
        fourcc: u32,
        modifier: u64,
        width: u32,
        height: u32,
        slot_strides: [u32; 2],
        slot_offsets: [u32; 2],
        slot_sizes: [u64; 2],
        color_seed: u32,
        frame_count: u32,
        // Cross-vendor (AMD↔NVIDIA) OPAQUE_FD timeline import isn't
        // interoperable; on cross_gpu the orchestrator drops timelines
        // and the socket roundtrip becomes the per-frame fence.
        #[serde(default = "default_true")]
        use_timelines: bool,
    },
    BindTimelines,
    Frame {
        n: u32,
        slot: u32,
        acquire_value: u64,
        release_value: u64,
    },
    ColorReport {
        n: u32,
        slot: u32,
        expected_rgba: u32,
        got_rgba: u32,
        ok: bool,
    },
    LoopDone,

    Ack,
    Bye {
        reason: String,
    },
    Panic {
        phase: String,
        frame: Option<u32>,
        message: String,
    },
}

const OPCODE: u16 = 0x4254;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Nix(nix::errno::Errno),
    PeerClosed,
    BadOpcode(u16),
    BadFrameLen(u16),
    BodyTooLarge(usize),
    Json(serde_json::Error),
    TooManyFds(usize),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Nix(e) => write!(f, "syscall: {e}"),
            Self::PeerClosed => write!(f, "peer closed"),
            Self::BadOpcode(o) => write!(f, "bad opcode: {o:#x}"),
            Self::BadFrameLen(n) => write!(f, "bad frame len: {n}"),
            Self::BodyTooLarge(n) => write!(f, "body too large: {n}B"),
            Self::Json(e) => write!(f, "json: {e}"),
            Self::TooManyFds(n) => write!(f, "too many fds: {n}"),
        }
    }
}
impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<nix::errno::Errno> for Error {
    fn from(e: nix::errno::Errno) -> Self {
        Self::Nix(e)
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn send_msg(sock: &UnixStream, msg: &TestMsg, fds: &[RawFd]) -> Result<()> {
    if fds.len() > MAX_FDS {
        return Err(Error::TooManyFds(fds.len()));
    }
    let body = serde_json::to_vec(msg)?;
    if body.len() > MAX_BODY_BYTES {
        return Err(Error::BodyTooLarge(body.len()));
    }
    let total = (body.len() + 4) as u16;
    let mut framed = Vec::with_capacity(body.len() + 4);
    framed.extend_from_slice(&OPCODE.to_le_bytes());
    framed.extend_from_slice(&total.to_le_bytes());
    framed.extend_from_slice(&body);
    let iov = [IoSlice::new(&framed)];
    let cmsgs_storage;
    let cmsgs: &[ControlMessage] = if fds.is_empty() {
        &[]
    } else {
        cmsgs_storage = [ControlMessage::ScmRights(fds)];
        &cmsgs_storage
    };
    loop {
        // MSG_NOSIGNAL: peer can die mid-send; we surface EPIPE as Err
        // instead of taking SIGPIPE.
        match sendmsg::<()>(sock.as_raw_fd(), &iov, cmsgs, MsgFlags::MSG_NOSIGNAL, None) {
            Ok(n) if n == framed.len() => return Ok(()),
            Ok(_n) => return Err(Error::Io(std::io::ErrorKind::WriteZero.into())),
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => return Err(Error::Nix(e)),
        }
    }
}

pub fn recv_msg(sock: &UnixStream) -> Result<(TestMsg, Vec<OwnedFd>)> {
    let mut hdr = [0u8; 4];
    let mut fds: Vec<OwnedFd> = Vec::new();
    let mut filled = 0usize;
    while filled < 4 {
        let mut cmsg_space = nix::cmsg_space!([RawFd; MAX_FDS]);
        let mut iov = [IoSliceMut::new(&mut hdr[filled..])];
        let msg = loop {
            match recvmsg::<()>(
                sock.as_raw_fd(),
                &mut iov,
                Some(&mut cmsg_space),
                MsgFlags::empty(),
            ) {
                Ok(m) => break m,
                Err(nix::errno::Errno::EINTR) => continue,
                Err(e) => return Err(Error::Nix(e)),
            }
        };
        for c in msg.cmsgs().map_err(Error::Nix)? {
            if let ControlMessageOwned::ScmRights(rfds) = c {
                for fd in rfds {
                    fds.push(unsafe { OwnedFd::from_raw_fd(fd) });
                }
            }
        }
        if msg.bytes == 0 {
            return Err(Error::PeerClosed);
        }
        filled += msg.bytes;
    }
    let opcode = u16::from_le_bytes([hdr[0], hdr[1]]);
    if opcode != OPCODE {
        return Err(Error::BadOpcode(opcode));
    }
    let total = u16::from_le_bytes([hdr[2], hdr[3]]);
    if total < 4 {
        return Err(Error::BadFrameLen(total));
    }
    let body_len = (total - 4) as usize;
    let mut body = vec![0u8; body_len];
    let mut s = sock;
    s.read_exact(&mut body)?;
    let msg: TestMsg = serde_json::from_slice(&body)?;
    Ok((msg, fds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_roundtrips_over_socketpair_with_fd() {
        use nix::sys::memfd::{memfd_create, MemFdCreateFlag};
        use std::ffi::CString;
        let (a, b) = UnixStream::pair().unwrap();
        let memfd = memfd_create(
            &CString::new("self-test-roundtrip").unwrap(),
            MemFdCreateFlag::MFD_CLOEXEC,
        )
        .unwrap();

        let msg = TestMsg::ProbeModifier {
            fourcc: 0x34324241,
            modifier: 0x0100000000000001,
            width: 1024,
            height: 1024,
            plane_stride: 4096,
            plane_offset: 0,
            plane_size: 4096 * 1024,
        };
        send_msg(&a, &msg, &[memfd.as_raw_fd()]).unwrap();
        drop(memfd);
        let (got, fds) = recv_msg(&b).unwrap();
        assert_eq!(got, msg);
        assert_eq!(fds.len(), 1);
    }

    #[test]
    fn peer_close_surfaces_eof() {
        let (a, b) = UnixStream::pair().unwrap();
        drop(a);
        let err = recv_msg(&b).unwrap_err();
        assert!(matches!(err, Error::PeerClosed));
    }
}
