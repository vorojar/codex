//! Short, best-effort terminal response probes.
//!
//! Crossterm's public helpers wait up to two seconds for terminal responses. That is too long for
//! TUI startup, where unsupported terminals should simply fall back to conservative defaults.

#[cfg(unix)]
#[cfg_attr(test, allow(dead_code))]
mod imp {
    use std::fs::File;
    use std::fs::OpenOptions;
    use std::io;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::time::Duration;
    use std::time::Instant;

    use crossterm::event::KeyboardEnhancementFlags;
    use ratatui::layout::Position;

    pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_millis(100);

    #[derive(Debug, Clone, Copy, Eq, PartialEq)]
    pub(crate) struct DefaultColors {
        pub(crate) fg: (u8, u8, u8),
        pub(crate) bg: (u8, u8, u8),
    }

    struct Tty {
        file: File,
        original_flags: libc::c_int,
    }

    impl Tty {
        fn open() -> io::Result<Self> {
            let file = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
            let fd = file.as_raw_fd();
            let original_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if original_flags == -1 {
                return Err(io::Error::last_os_error());
            }
            if unsafe { libc::fcntl(fd, libc::F_SETFL, original_flags | libc::O_NONBLOCK) } == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                file,
                original_flags,
            })
        }

        fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.file.write_all(bytes)?;
            self.file.flush()
        }

        fn read_available(&mut self, buffer: &mut Vec<u8>) -> io::Result<()> {
            let mut chunk = [0_u8; 256];
            loop {
                let count = unsafe {
                    libc::read(
                        self.file.as_raw_fd(),
                        chunk.as_mut_ptr().cast::<libc::c_void>(),
                        chunk.len(),
                    )
                };
                if count > 0 {
                    buffer.extend_from_slice(&chunk[..count as usize]);
                    continue;
                }
                if count == 0 {
                    return Ok(());
                }
                let err = io::Error::last_os_error();
                if matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) {
                    return Ok(());
                }
                return Err(err);
            }
        }

        fn poll_readable(&self, timeout: Duration) -> io::Result<bool> {
            let mut fd = libc::pollfd {
                fd: self.file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let timeout_ms = timeout.as_millis().min(libc::c_int::MAX as u128) as libc::c_int;
            loop {
                let result = unsafe {
                    libc::poll(&mut fd, /*nfds*/ 1, timeout_ms)
                };
                if result > 0 {
                    return Ok((fd.revents & libc::POLLIN) != 0);
                }
                if result == 0 {
                    return Ok(false);
                }
                let err = io::Error::last_os_error();
                if err.kind() != io::ErrorKind::Interrupted {
                    return Err(err);
                }
            }
        }
    }

    impl Drop for Tty {
        fn drop(&mut self) {
            let _ =
                unsafe { libc::fcntl(self.file.as_raw_fd(), libc::F_SETFL, self.original_flags) };
        }
    }

    pub(crate) fn cursor_position(timeout: Duration) -> io::Result<Option<Position>> {
        let mut tty = Tty::open()?;
        tty.write_all(b"\x1B[6n")?;
        let Some(response) = read_until(&mut tty, timeout, parse_cursor_position)? else {
            return Ok(None);
        };
        Ok(Some(response))
    }

    pub(crate) fn default_colors(timeout: Duration) -> io::Result<Option<DefaultColors>> {
        let mut tty = Tty::open()?;
        let deadline = Instant::now() + timeout;
        let Some(fg) = query_color_slot(&mut tty, /*slot*/ 10, remaining(deadline))? else {
            return Ok(None);
        };
        let Some(bg) = query_color_slot(&mut tty, /*slot*/ 11, remaining(deadline))? else {
            return Ok(None);
        };
        Ok(Some(DefaultColors { fg, bg }))
    }

    pub(crate) fn keyboard_enhancement_supported(timeout: Duration) -> io::Result<Option<bool>> {
        let mut tty = Tty::open()?;
        tty.write_all(b"\x1B[?u\x1B[c")?;
        read_until(&mut tty, timeout, parse_keyboard_enhancement_support)
    }

    fn query_color_slot(
        tty: &mut Tty,
        slot: u8,
        timeout: Duration,
    ) -> io::Result<Option<(u8, u8, u8)>> {
        write!(tty.file, "\x1B]{slot};?\x1B\\")?;
        tty.file.flush()?;
        read_until(tty, timeout, |buffer| parse_osc_color(buffer, slot))
    }

    fn read_until<T>(
        tty: &mut Tty,
        timeout: Duration,
        mut parse: impl FnMut(&[u8]) -> Option<T>,
    ) -> io::Result<Option<T>> {
        let deadline = Instant::now() + timeout;
        let mut buffer = Vec::new();
        loop {
            tty.read_available(&mut buffer)?;
            if let Some(value) = parse(&buffer) {
                return Ok(Some(value));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            if !tty.poll_readable(deadline.saturating_duration_since(now))? {
                return Ok(None);
            }
        }
    }

    fn remaining(deadline: Instant) -> Duration {
        deadline.saturating_duration_since(Instant::now())
    }

    fn parse_cursor_position(buffer: &[u8]) -> Option<Position> {
        for start in find_all_subslices(buffer, b"\x1B[") {
            let rest = &buffer[start + 2..];
            let Some(end) = rest.iter().position(|b| *b == b'R') else {
                continue;
            };
            let payload = std::str::from_utf8(&rest[..end]).ok()?;
            let (row, col) = payload.split_once(';')?;
            let row = row.parse::<u16>().ok()?.saturating_sub(1);
            let col = col.parse::<u16>().ok()?.saturating_sub(1);
            return Some(Position { x: col, y: row });
        }
        None
    }

    fn parse_osc_color(buffer: &[u8], slot: u8) -> Option<(u8, u8, u8)> {
        let prefix = format!("\x1B]{slot};");
        let start = find_subslice(buffer, prefix.as_bytes())?;
        let payload_start = start + prefix.len();
        let rest = &buffer[payload_start..];
        let (payload_end, _terminator_len) = osc_payload_end(rest)?;
        let payload = std::str::from_utf8(&rest[..payload_end]).ok()?;
        parse_osc_rgb(payload)
    }

    fn osc_payload_end(buffer: &[u8]) -> Option<(usize, usize)> {
        let mut idx = 0;
        while idx < buffer.len() {
            match buffer[idx] {
                0x07 => return Some((idx, 1)),
                0x1B if buffer.get(idx + 1) == Some(&b'\\') => return Some((idx, 2)),
                _ => idx += 1,
            }
        }
        None
    }

    fn parse_osc_rgb(payload: &str) -> Option<(u8, u8, u8)> {
        let (prefix, values) = payload.trim().split_once(':')?;
        if !prefix.eq_ignore_ascii_case("rgb") && !prefix.eq_ignore_ascii_case("rgba") {
            return None;
        }

        let mut parts = values.split('/');
        let r = parse_osc_component(parts.next()?)?;
        let g = parse_osc_component(parts.next()?)?;
        let b = parse_osc_component(parts.next()?)?;
        if prefix.eq_ignore_ascii_case("rgba") {
            parse_osc_component(parts.next()?)?;
        }
        parts.next().is_none().then_some((r, g, b))
    }

    fn parse_osc_component(component: &str) -> Option<u8> {
        match component.len() {
            2 => u8::from_str_radix(component, 16).ok(),
            4 => u16::from_str_radix(component, 16)
                .ok()
                .map(|value| (value / 257) as u8),
            _ => None,
        }
    }

    fn parse_keyboard_enhancement_support(buffer: &[u8]) -> Option<bool> {
        if find_keyboard_flags(buffer).is_some() {
            return Some(true);
        }
        find_primary_device_attributes(buffer).map(|_| false)
    }

    fn find_keyboard_flags(buffer: &[u8]) -> Option<KeyboardEnhancementFlags> {
        for start in find_all_subslices(buffer, b"\x1B[?") {
            let rest = &buffer[start + 3..];
            let Some(end) = rest.iter().position(|b| *b == b'u') else {
                continue;
            };
            if end == 0 {
                continue;
            }
            let bits = std::str::from_utf8(&rest[..end]).ok()?.parse::<u8>().ok()?;
            let mut flags = KeyboardEnhancementFlags::empty();
            if bits & 1 != 0 {
                flags |= KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES;
            }
            if bits & 2 != 0 {
                flags |= KeyboardEnhancementFlags::REPORT_EVENT_TYPES;
            }
            if bits & 4 != 0 {
                flags |= KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS;
            }
            if bits & 8 != 0 {
                flags |= KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES;
            }
            return Some(flags);
        }
        None
    }

    fn find_primary_device_attributes(buffer: &[u8]) -> Option<()> {
        for start in find_all_subslices(buffer, b"\x1B[?") {
            let rest = &buffer[start + 3..];
            let Some(end) = rest.iter().position(|b| *b == b'c') else {
                continue;
            };
            if end > 0 && rest[..end].iter().all(|b| b.is_ascii_digit() || *b == b';') {
                return Some(());
            }
        }
        None
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    fn find_all_subslices<'a>(
        haystack: &'a [u8],
        needle: &'a [u8],
    ) -> impl Iterator<Item = usize> + 'a {
        haystack
            .windows(needle.len())
            .enumerate()
            .filter_map(move |(idx, window)| (window == needle).then_some(idx))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_cursor_position_as_zero_based() {
            assert_eq!(
                parse_cursor_position(b"\x1B[20;10R"),
                Some(Position { x: 9, y: 19 })
            );
        }

        #[test]
        fn parses_osc_colors_with_bel_and_st() {
            assert_eq!(
                parse_osc_color(b"\x1B]10;rgb:ffff/8000/0000\x07", /*slot*/ 10),
                Some((255, 127, 0))
            );
            assert_eq!(
                parse_osc_color(b"\x1B]11;rgba:00/80/ff/ff\x1B\\", /*slot*/ 11),
                Some((0, 128, 255))
            );
        }

        #[test]
        fn parses_keyboard_enhancement_flags_and_pda_fallback() {
            assert_eq!(parse_keyboard_enhancement_support(b"\x1B[?7u"), Some(true));
            assert_eq!(
                parse_keyboard_enhancement_support(b"\x1B[?64;1;2c"),
                Some(false)
            );
            assert_eq!(parse_keyboard_enhancement_support(b""), None);
        }
    }
}

#[cfg(unix)]
pub(crate) use imp::*;

#[cfg(not(unix))]
pub(crate) const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(100);
