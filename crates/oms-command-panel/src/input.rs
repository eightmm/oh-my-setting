use crate::{Error, Result, require};
use rustix::fs::{Mode, OFlags};
use rustix::termios::{LocalModes, OptionalActions, SpecialCodeIndex, Termios};
use std::fs::File;
use std::io::{IsTerminal, Read, Write};
use std::time::{Duration, Instant};

const ESCAPE_GRACE: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Escape,
    Tab,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Repaint,
    Function(u8),
    Click(u16, u16),
    Drag(u16),
    Release(u16),
    Scroll(i32),
    Paste,
    Eof,
    Unbound,
}
#[derive(Default)]
pub struct Decoder {
    pending: Vec<u8>,
    paste: bool,
    escape_since: Option<Instant>,
}
impl Decoder {
    pub fn feed(&mut self, data: &[u8]) -> Vec<Key> {
        self.pending.extend_from_slice(data);
        let mut keys = Vec::new();
        loop {
            if self.pending.is_empty() {
                self.escape_since = None;
                break;
            }
            if self.paste {
                if let Some(end) = self.pending.windows(6).position(|v| v == b"\x1b[201~") {
                    self.pending.drain(..end + 6);
                    self.paste = false;
                } else {
                    let remove = self.pending.len().saturating_sub(5);
                    self.pending.drain(..remove);
                    break;
                }
                continue;
            }
            if self.pending.starts_with(b"\x1b[200~") {
                self.pending.drain(..6);
                self.escape_since = None;
                self.paste = true;
                keys.push(Key::Paste);
                continue;
            }
            if self.pending[0] == 27 {
                if self.pending.len() == 1 {
                    self.escape_since.get_or_insert_with(Instant::now);
                    break;
                }
                let mut used = 0;
                let mut found = Key::Unbound;
                for (sequence, key) in [
                    (b"\x1b[A".as_slice(), Key::Up),
                    (b"\x1b[B", Key::Down),
                    (b"\x1b[C", Key::Right),
                    (b"\x1b[D", Key::Left),
                    (b"\x1bOA", Key::Up),
                    (b"\x1bOB", Key::Down),
                    (b"\x1bOC", Key::Right),
                    (b"\x1bOD", Key::Left),
                    (b"\x1b[H", Key::Home),
                    (b"\x1b[F", Key::End),
                    (b"\x1bOH", Key::Home),
                    (b"\x1bOF", Key::End),
                    (b"\x1b[1~", Key::Home),
                    (b"\x1b[4~", Key::End),
                    (b"\x1b[7~", Key::Home),
                    (b"\x1b[8~", Key::End),
                    (b"\x1b[5~", Key::PageUp),
                    (b"\x1b[6~", Key::PageDown),
                    (b"\x1b[15~", Key::Function(5)),
                    (b"\x1b[19~", Key::Function(8)),
                    (b"\x1b[20~", Key::Function(9)),
                    (b"\x1b[24~", Key::Function(12)),
                ] {
                    if self.pending.starts_with(sequence) {
                        used = sequence.len();
                        found = key;
                        break;
                    }
                }
                if used == 0
                    && self.pending.starts_with(b"\x1b[<")
                    && let Some(end) = self.pending.iter().position(|c| matches!(c, b'M' | b'm'))
                {
                    let values = std::str::from_utf8(&self.pending[3..end])
                        .unwrap_or("")
                        .split(';')
                        .map(str::parse::<u16>)
                        .collect::<std::result::Result<Vec<_>, _>>();
                    if let Ok(v) = values
                        && v.len() == 3
                        && v[1] > 0
                        && v[2] > 0
                    {
                        found = match (v[0], self.pending[end]) {
                            (0, b'M') => Key::Click(v[1], v[2]),
                            (32, b'M') => Key::Drag(v[2]),
                            (0 | 3, b'm') => Key::Release(v[2]),
                            (64, b'M') => Key::Scroll(-3),
                            (65, b'M') => Key::Scroll(3),
                            _ => Key::Unbound,
                        };
                    }
                    used = end + 1;
                }
                if used == 0 && matches!(self.pending[1], b'[' | b'O') {
                    if let Some(end) = self
                        .pending
                        .iter()
                        .enumerate()
                        .skip(2)
                        .find_map(|(i, c)| (0x40..=0x7e).contains(c).then_some(i))
                    {
                        used = end + 1;
                    } else if self.pending.len() < 64 {
                        self.escape_since.get_or_insert_with(Instant::now);
                        break;
                    } else {
                        used = self.pending.len();
                    }
                }
                if used == 0 {
                    used = 2;
                }
                self.pending.drain(..used);
                self.escape_since = None;
                keys.push(found);
                continue;
            }
            let byte = self.pending[0];
            let size = match byte {
                0..=0x7f => 1,
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => 1,
            };
            if self.pending.len() < size {
                break;
            }
            let key = match byte {
                b'\r' | b'\n' => Key::Enter,
                b'\t' => Key::Tab,
                8 | 127 => Key::Backspace,
                12 => Key::Repaint,
                4 => Key::Eof,
                _ => std::str::from_utf8(&self.pending[..size])
                    .ok()
                    .and_then(|v| v.chars().next())
                    .filter(|c| !c.is_control())
                    .map(Key::Char)
                    .unwrap_or(Key::Unbound),
            };
            self.pending.drain(..size);
            keys.push(key);
        }
        keys
    }
    fn escape_remaining(&self, now: Instant) -> Option<Duration> {
        self.escape_since
            .map(|since| ESCAPE_GRACE.saturating_sub(now.saturating_duration_since(since)))
    }
    fn poll_duration(&self, duration: Duration, now: Instant) -> Duration {
        let wait = duration.min(crate::wakeup::SLICE);
        wait.min(self.escape_remaining(now).unwrap_or(wait))
    }
    pub fn expire(&mut self) -> Option<Key> {
        if self.escape_remaining(Instant::now()) == Some(Duration::ZERO) {
            let bare = self.pending == b"\x1b";
            self.pending.clear();
            self.escape_since = None;
            Some(if bare { Key::Escape } else { Key::Unbound })
        } else {
            None
        }
    }
}

/// Owns a reopened terminal description and restores the original mode on return.
/// ISIG stays enabled: the embedding host owns signal handling and interruption.
pub struct TerminalInput {
    file: File,
    original: Termios,
    decoder: Decoder,
}
impl TerminalInput {
    pub fn open() -> Result<Option<Self>> {
        let stdin = std::io::stdin();
        if !stdin.is_terminal()
            || !std::io::stdout().is_terminal()
            || std::env::var("TERM").is_ok_and(|v| v == "dumb")
        {
            return Ok(None);
        }
        require(
            rustix::termios::tcgetpgrp(&stdin).map_err(Error::new)? == rustix::process::getpgrp(),
            "stdin is not a foreground terminal",
        )?;
        let name = rustix::termios::ttyname(&stdin, Vec::new()).map_err(Error::new)?;
        let fd = rustix::fs::open(
            &name,
            OFlags::RDWR | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(Error::new)?;
        let before = rustix::fs::fstat(&stdin).map_err(Error::new)?;
        let after = rustix::fs::fstat(&fd).map_err(Error::new)?;
        require(
            before.st_dev == after.st_dev
                && before.st_ino == after.st_ino
                && before.st_rdev == after.st_rdev,
            "terminal identity changed",
        )?;
        let file = File::from(fd);
        let original = rustix::termios::tcgetattr(&file).map_err(Error::new)?;
        let mut changed = original.clone();
        changed
            .local_modes
            .remove(LocalModes::ICANON | LocalModes::ECHO);
        changed.special_codes[SpecialCodeIndex::VMIN] = 0;
        changed.special_codes[SpecialCodeIndex::VTIME] = 0;
        let mut guard = Self {
            file,
            original,
            decoder: Decoder::default(),
        };
        rustix::termios::tcsetattr(&guard.file, OptionalActions::Now, &changed)
            .map_err(Error::new)?;
        guard
            .file
            .write_all(b"\x1b[?1049h\x1b[?1000h\x1b[?1006h\x1b[?2004h\x1b[?25l")
            .map_err(Error::new)?;
        Ok(Some(guard))
    }
    pub fn enable_drag(&mut self) -> Result<()> {
        self.file.write_all(b"\x1b[?1002h").map_err(Error::new)
    }
    pub fn wait(&mut self, duration: Duration) -> Result<Vec<Key>> {
        self.wait_woken(duration, None).map(|(keys, _)| keys)
    }
    /// Like [`Self::wait`], also polling the wakeup socket; the flag is true
    /// when a producer woke the watcher (the datagram burst is consumed).
    pub fn wait_woken(
        &mut self,
        duration: Duration,
        wake: Option<&crate::wakeup::Wakeup>,
    ) -> Result<(Vec<Key>, bool)> {
        use rustix::event::{PollFd, PollFlags, poll};
        require(
            rustix::termios::tcgetpgrp(&self.file).map_err(Error::new)?
                == rustix::process::getpgrp(),
            "terminal foreground ownership changed",
        )?;
        let duration = self.decoder.poll_duration(duration, Instant::now());
        let timeout = crate::wakeup::timespec(duration);
        let wake_fd = wake.map(|w| w.fd());
        let mut descriptors = vec![PollFd::new(&self.file, PollFlags::IN)];
        if let Some(fd) = &wake_fd {
            descriptors.push(PollFd::new(fd, PollFlags::IN));
        }
        match poll(&mut descriptors, Some(&timeout)) {
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => return Ok((vec![], false)),
            Err(e) => return Err(Error::new(e)),
        }
        let readable = descriptors[0].revents().contains(PollFlags::IN);
        let wake_ready = descriptors
            .get(1)
            .is_some_and(|d| d.revents().contains(PollFlags::IN));
        let woken = wake_ready
            && wake.is_some_and(|w| {
                let burst = crate::wakeup::COALESCE;
                // A late wake must not extend the pending input deadline.
                if self.decoder.poll_duration(burst, Instant::now()) < burst {
                    w.drain()
                } else {
                    w.drain_burst()
                }
            });
        let mut output = Vec::new();
        if readable {
            let mut data = [0; 4096];
            match self.file.read(&mut data) {
                Ok(0) => output.push(Key::Eof),
                Ok(n) => output.extend(self.decoder.feed(&data[..n])),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(Error::new(e)),
            }
        }
        if let Some(key) = self.decoder.expire() {
            output.push(key);
        }
        Ok((output, woken))
    }
}
impl Drop for TerminalInput {
    fn drop(&mut self) {
        let _ = self
            .file
            .write_all(b"\x1b[?1002l\x1b[?1000l\x1b[?1006l\x1b[?2004l\x1b[?25h\x1b[0m\x1b[?1049l");
        let _ = rustix::termios::tcsetattr(&self.file, OptionalActions::Now, &self.original);
    }
}

#[cfg(test)]
mod tests {
    use super::{Decoder, Duration, Instant, Key};

    #[test]
    fn wake_repoll_uses_remaining_escape_grace() {
        let mut decoder = Decoder::default();
        let start = Instant::now();
        let slice = Duration::from_millis(100);
        assert_eq!(decoder.poll_duration(slice * 2, start), slice);
        assert!(decoder.feed(b"\x1b").is_empty());
        decoder.escape_since = Some(start);

        // The first poll sees a wake 60 ms into the grace. The next poll
        // must reach the original deadline even if the caller asks for 100 ms.
        let woke = start + Duration::from_millis(60);
        assert_eq!(
            decoder.poll_duration(slice, woke),
            Duration::from_millis(40)
        );
        assert_eq!(
            decoder.poll_duration(Duration::from_millis(20), woke),
            Duration::from_millis(20)
        );
        assert_eq!(decoder.poll_duration(slice, start + slice), Duration::ZERO);

        let soon_due = start + Duration::from_millis(95);
        let burst = crate::wakeup::COALESCE;
        assert_eq!(
            decoder.poll_duration(burst, soon_due),
            Duration::from_millis(5)
        );
    }

    #[test]
    fn split_escape_continuations_keep_their_key_class() {
        for (tail, key) in [
            (b"[A".as_slice(), Key::Up),
            (b"[<0;15;24M", Key::Click(15, 24)),
            (b"q", Key::Unbound),
        ] {
            let mut decoder = Decoder::default();
            assert!(decoder.feed(b"\x1b").is_empty());
            assert_eq!(decoder.feed(tail), [key]);
            assert!(decoder.escape_since.is_none());
        }
    }

    #[test]
    fn split_paste_start_clears_escape_deadline() {
        let mut decoder = Decoder::default();
        assert!(decoder.feed(b"\x1b").is_empty());
        assert_eq!(decoder.feed(b"[200~q\x1b[20"), [Key::Paste]);
        assert!(decoder.escape_since.is_none());
        assert_eq!(decoder.feed(b"1~z"), [Key::Char('z')]);
    }

    #[test]
    fn sgr_mouse_drag_and_release_keep_scroll_and_click() {
        let mut decoder = Decoder::default();
        assert!(decoder.feed(b"\x1b[<0;15;").is_empty());
        assert_eq!(
            decoder.feed(b"24M\x1b[<32;16;17M\x1b[<0;16;17m\x1b[<64;16;17M"),
            [
                Key::Click(15, 24),
                Key::Drag(17),
                Key::Release(17),
                Key::Scroll(-3)
            ],
        );
    }
}
