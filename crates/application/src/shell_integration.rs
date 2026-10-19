//! Shell integration: reading the marks a shell writes about its prompt and commands.
//!
//! A shell that was started with the integration scripts prints private escape sequences
//! (`ESC ] 633 ; ... BEL`, the ones VS Code uses) at the start of its prompt, when a command line
//! is accepted, when it starts to run, and when it ends. `MarkScanner` reads them out of the byte
//! stream and tells what happened; the text itself is left for the terminal.

/// What the marks of a shell say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellEvent {
    /// The first mark: the shell is running the integration.
    Integrated,
    /// The shell is in this folder.
    Cwd(String),
    /// The command line that is about to run. `trusted` is true when the shell proved it wrote it
    /// (the nonce matched), so a program printing the mark cannot pass for the shell.
    CommandLine { text: String, trusted: bool },
    /// The command started to run.
    Started,
    /// Text the running command printed.
    Output(Vec<u8>),
    /// The command ended.
    Ended { code: Option<i32> },
}

#[derive(Debug, Default)]
pub struct MarkScanner {
    /// The nonce the shell was given; a command line mark carries it back.
    nonce: Option<String>,
    /// An escape sequence that is not complete yet.
    partial: Vec<u8>,
    in_sequence: bool,
    integrated: bool,
    running: bool,
}

/// Longer than any mark; a sequence that goes on is not one of ours.
const LONGEST_MARK: usize = 64 * 1024;

impl MarkScanner {
    pub fn new(nonce: Option<String>) -> Self {
        Self { nonce, ..Default::default() }
    }

    /// The nonce marks of command lines must carry.
    pub fn nonce(&self) -> Option<&str> {
        self.nonce.as_deref()
    }

    /// Whether the shell has shown any mark.
    pub fn is_integrated(&self) -> bool {
        self.integrated
    }

    /// Read `bytes`, the next output of the shell, and say what the marks in it mean.
    pub fn scan(&mut self, bytes: &[u8]) -> Vec<ShellEvent> {
        let mut events = Vec::new();
        let mut output: Vec<u8> = Vec::new();
        for &byte in bytes {
            if self.in_sequence {
                self.partial.push(byte);
                if self.partial.len() == 2 {
                    // Only `ESC ]` starts an operating system command; `ESC` with anything else
                    // (`ESC [`, `ESC \`, `ESC BEL`) is for the terminal.
                    if byte != b']' {
                        let whole = std::mem::take(&mut self.partial);
                        self.in_sequence = false;
                        self.pass(&whole, &mut output);
                    }
                    continue;
                }
                let done = match byte {
                    0x07 => Some(1),
                    b'\\' if self.partial[self.partial.len() - 2] == 0x1b => Some(2),
                    _ => None,
                };
                if let Some(terminator) = done {
                    let body = self.partial[2..self.partial.len() - terminator].to_vec();
                    let whole = std::mem::take(&mut self.partial);
                    self.in_sequence = false;
                    match body.strip_prefix(b"633;") {
                        Some(mark) => {
                            self.flush(&mut output, &mut events);
                            self.mark(mark, &mut events);
                        }
                        // Some other program's sequence: not ours, the terminal reads it.
                        None => self.pass(&whole, &mut output),
                    }
                } else if self.partial.len() > LONGEST_MARK {
                    let whole = std::mem::take(&mut self.partial);
                    self.in_sequence = false;
                    self.pass(&whole, &mut output);
                }
            } else if byte == 0x1b {
                self.in_sequence = true;
                self.partial.push(byte);
            } else {
                self.pass(&[byte], &mut output);
            }
        }
        self.flush(&mut output, &mut events);
        events
    }

    fn pass(&self, bytes: &[u8], output: &mut Vec<u8>) {
        if self.running {
            output.extend_from_slice(bytes);
        }
    }

    fn flush(&self, output: &mut Vec<u8>, events: &mut Vec<ShellEvent>) {
        if !output.is_empty() {
            events.push(ShellEvent::Output(std::mem::take(output)));
        }
    }

    fn mark(&mut self, mark: &[u8], events: &mut Vec<ShellEvent>) {
        let text = String::from_utf8_lossy(mark).into_owned();
        let (kind, rest) = text.split_once(';').map_or((text.as_str(), ""), |(k, r)| (k, r));
        if !self.integrated {
            self.integrated = true;
            events.push(ShellEvent::Integrated);
        }
        match kind {
            "P" => {
                if let Some(cwd) = rest.strip_prefix("Cwd=") {
                    events.push(ShellEvent::Cwd(unescape(cwd)));
                }
            }
            "E" => {
                // `E;command;nonce`: the command can hold escaped semicolons, never a bare one.
                let (command, given) = rest.split_once(';').map_or((rest, None), |(c, n)| (c, Some(n)));
                let trusted = match (&self.nonce, given) {
                    (Some(wanted), Some(given)) => wanted == given,
                    (None, _) => true,
                    _ => false,
                };
                events.push(ShellEvent::CommandLine { text: unescape(command), trusted });
            }
            "C" => {
                self.running = true;
                events.push(ShellEvent::Started);
            }
            "D" => {
                if self.running {
                    self.running = false;
                    events.push(ShellEvent::Ended { code: rest.trim().parse().ok() });
                }
            }
            _ => {}
        }
    }
}

/// Undo the escapes the scripts put in (`\\`, and `\xNN` for what could not be written plainly).
pub fn unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            if bytes[i + 1] == b'\\' {
                out.push(b'\\');
                i += 2;
                continue;
            }
            if bytes[i + 1] == b'x' && i + 4 <= bytes.len() {
                if let Some(value) = text.get(i + 2..i + 4).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    out.push(value);
                    i += 4;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(body: &str) -> Vec<u8> {
        format!("\x1b]633;{body}\x07").into_bytes()
    }

    #[test]
    fn a_command_is_followed_from_its_line_to_its_end() {
        let mut scanner = MarkScanner::new(Some("n0nce".into()));
        let mut bytes = Vec::new();
        bytes.extend(mark("A"));
        bytes.extend(b"$ ");
        bytes.extend(mark("B"));
        bytes.extend(mark("P;Cwd=/work/my\\x20dir"));
        bytes.extend(mark("E;echo hi\\x3b ls;n0nce"));
        bytes.extend(mark("C"));
        bytes.extend(b"hi\r\n");
        bytes.extend(mark("D;3"));
        let events = scanner.scan(&bytes);
        assert_eq!(
            events,
            vec![
                ShellEvent::Integrated,
                ShellEvent::Cwd("/work/my dir".into()),
                ShellEvent::CommandLine { text: "echo hi; ls".into(), trusted: true },
                ShellEvent::Started,
                ShellEvent::Output(b"hi\r\n".to_vec()),
                ShellEvent::Ended { code: Some(3) },
            ]
        );
        assert!(scanner.is_integrated());
    }

    #[test]
    fn a_mark_cut_between_two_reads_is_still_read() {
        let mut scanner = MarkScanner::new(None);
        let whole = mark("C");
        let (first, second) = whole.split_at(4);
        assert_eq!(scanner.scan(first), vec![]);
        assert_eq!(scanner.scan(second), vec![ShellEvent::Integrated, ShellEvent::Started]);
    }

    #[test]
    fn a_line_with_a_wrong_nonce_is_not_trusted_and_other_sequences_stay_in_the_output() {
        let mut scanner = MarkScanner::new(Some("right".into()));
        let mut bytes = Vec::new();
        bytes.extend(mark("E;rm -rf x;wrong"));
        bytes.extend(mark("C"));
        bytes.extend(b"\x1b]0;a title\x07\x1b[31mred\x1b[0m");
        bytes.extend(b"\x1b]633;D;0\x1b\\");
        let events = scanner.scan(&bytes);
        assert!(events.contains(&ShellEvent::CommandLine { text: "rm -rf x".into(), trusted: false }));
        let output: Vec<u8> = events.iter().filter_map(|e| if let ShellEvent::Output(o) = e { Some(o.clone()) } else { None }).flatten().collect();
        assert_eq!(output, b"\x1b]0;a title\x07\x1b[31mred\x1b[0m");
        assert_eq!(events.last(), Some(&ShellEvent::Ended { code: Some(0) }));
    }

    #[test]
    fn an_escape_that_starts_no_command_is_not_taken_for_one() {
        let mut scanner = MarkScanner::new(None);
        // `ESC \` and `ESC BEL` with nothing before them, a color, and a mark after all of them.
        let mut bytes = Vec::new();
        bytes.extend(b"\x1b\\\x1b\x07\x1b[1m");
        bytes.extend(mark("C"));
        bytes.extend(b"out\x1bP+q\x1b\\");
        bytes.extend(mark("D;0"));
        let events = scanner.scan(&bytes);
        let output: Vec<u8> = events.iter().filter_map(|e| if let ShellEvent::Output(o) = e { Some(o.clone()) } else { None }).flatten().collect();
        assert_eq!(output, b"out\x1bP+q\x1b\\");
        assert!(events.contains(&ShellEvent::Started) && events.contains(&ShellEvent::Ended { code: Some(0) }), "{events:?}");
    }

    #[test]
    fn an_end_without_a_start_says_nothing() {
        let mut scanner = MarkScanner::new(None);
        assert_eq!(scanner.scan(&mark("D;0")), vec![ShellEvent::Integrated]);
    }

    #[test]
    fn escapes_are_undone() {
        assert_eq!(unescape("a\\\\b\\x0ac"), "a\\b\nc");
        assert_eq!(unescape("tail\\x"), "tail\\x");
    }
}
