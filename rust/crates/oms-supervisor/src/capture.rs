//! Bounded captures retain the beginning and end without bounding pipe drainage.
use sha2::{Digest, Sha256};
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitAction {
    Drain,
    Terminate,
}
#[derive(Clone, Debug)]
pub struct CaptureOptions {
    pub max_bytes: usize,
    pub on_limit: LimitAction,
    /// An explicit consumer cutoff; None drains until real EOF or cancellation.
    pub lines: Option<usize>,
}
impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            max_bytes: 8 * 1024 * 1024,
            on_limit: LimitAction::Drain,
            lines: None,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Captured {
    pub bytes: Vec<u8>,
    pub observed_bytes: u64,
    pub dropped_bytes: u64,
    pub sha256: String,
    pub eof: bool,
    pub limited: bool,
    pub cutoff: bool,
}
pub(crate) struct Capture {
    options: CaptureOptions,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: u64,
    hash: Sha256,
    lines: usize,
    cutoff: bool,
    pub eof: bool,
}
impl Capture {
    pub fn new(options: CaptureOptions) -> Self {
        let cutoff = options.lines == Some(0);
        Self {
            options,
            head: Vec::new(),
            tail: VecDeque::new(),
            total: 0,
            hash: Sha256::new(),
            lines: 0,
            cutoff,
            eof: false,
        }
    }
    pub fn push(&mut self, data: &[u8]) {
        if self.cutoff {
            return;
        }
        let mut end = data.len();
        if let Some(limit) = self.options.lines {
            for (i, byte) in data.iter().enumerate() {
                if self.lines == limit {
                    end = i;
                    self.cutoff = true;
                    break;
                }
                if *byte == b'\n' {
                    self.lines += 1;
                }
            }
        }
        let data = &data[..end];
        self.total = self.total.saturating_add(data.len() as u64);
        self.hash.update(data);
        let remaining = self.options.max_bytes.saturating_sub(self.head.len());
        self.head
            .extend_from_slice(&data[..data.len().min(remaining)]);
        let tail_limit = self.options.max_bytes.div_ceil(2);
        if data.len() >= tail_limit {
            self.tail.clear();
            self.tail.extend(&data[data.len() - tail_limit..]);
        } else {
            let excess = (self.tail.len() + data.len()).saturating_sub(tail_limit);
            self.tail.drain(..excess);
            self.tail.extend(data);
        }
    }
    pub fn prefix(&self) -> &[u8] {
        &self.head
    }
    pub fn should_stop(&self) -> bool {
        self.cutoff
            || self.options.on_limit == LimitAction::Terminate
                && self.total > self.options.max_bytes as u64
    }
    pub fn snapshot(&self) -> Captured {
        let limited = self.total > self.options.max_bytes as u64;
        let mut kept = self.head.clone();
        let mut dropped = 0;
        if limited {
            let tail: Vec<u8> = self.tail.iter().copied().collect();
            let terminal = if tail.ends_with(b"\n") {
                &b""[..]
            } else {
                &b"\n"[..]
            };
            let mut head_len = 0;
            loop {
                dropped = self.total.saturating_sub((head_len + tail.len()) as u64);
                let marker = format!(
                    "\n[oms: output truncated at log byte limit; {dropped} bytes dropped]\n"
                );
                // Very small generic captures cannot hold the canonical marker.
                if marker.len() + tail.len() + terminal.len() > self.options.max_bytes {
                    kept.truncate(self.options.max_bytes);
                    dropped = self.total.saturating_sub(kept.len() as u64);
                    break;
                }
                let next = self
                    .head
                    .len()
                    .min(self.options.max_bytes - marker.len() - tail.len() - terminal.len());
                if next == head_len {
                    kept.truncate(head_len);
                    kept.extend_from_slice(marker.as_bytes());
                    kept.extend_from_slice(&tail);
                    kept.extend_from_slice(terminal);
                    break;
                }
                head_len = next;
            }
        }
        Captured {
            bytes: kept,
            observed_bytes: self.total,
            dropped_bytes: dropped,
            sha256: format!("{:x}", self.hash.clone().finalize()),
            eof: self.eof,
            limited,
            cutoff: self.cutoff,
        }
    }
}
