//! Plain line-oriented frontend for pipes, prompt files, and non-TTY sessions.

use std::io::{self, BufRead, IsTerminal, Write};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use tokio_util::sync::CancellationToken;

use super::{MessageKind, OutputEvent, StreamKind, UserInterface};
use crate::risk::ApprovalChoice;

/// Terminal-free output adapter. ANSI styling is used only for a real TTY.
pub struct PlainUi {
    job_cancel: CancellationToken,
    output_error: Option<io::Error>,
    stream: Option<StreamKind>,
    prompt_visible: bool,
    stdout_styled: bool,
    stderr_styled: bool,
}

impl PlainUi {
    #[must_use]
    pub fn new() -> Self {
        Self {
            job_cancel: CancellationToken::new(),
            output_error: None,
            stream: None,
            prompt_visible: io::stdin().is_terminal() && io::stdout().is_terminal(),
            stdout_styled: io::stdout().is_terminal(),
            stderr_styled: io::stderr().is_terminal(),
        }
    }

    /// Read one prompt. Zero-byte reads are EOF, never an empty prompt retry.
    ///
    /// # Errors
    ///
    /// Returns input/output failures and `UnexpectedEof` when input closes.
    pub fn read_prompt(&mut self, prompt: &str) -> io::Result<String> {
        if let Some(error) = self.output_error.take() {
            return Err(error);
        }
        let stdin = io::stdin();
        let mut stdout = io::stdout().lock();
        let prompt = if self.prompt_visible { prompt } else { "" };
        read_prompt_from(&mut stdin.lock(), &mut stdout, prompt)
    }

    fn write_message(&mut self, kind: MessageKind, text: &str) -> io::Result<()> {
        self.close_stream()?;
        let (color, stderr) = match kind {
            MessageKind::Info | MessageKind::Stats => ("\x1b[2m", false),
            MessageKind::Warning => ("\x1b[33m", false),
            MessageKind::Error => ("\x1b[31m", true),
            MessageKind::Tool => ("\x1b[36m", false),
        };
        if stderr {
            write_line(&mut io::stderr().lock(), text, color, self.stderr_styled)
        } else {
            write_line(&mut io::stdout().lock(), text, color, self.stdout_styled)
        }
    }

    fn write_stream(&mut self, kind: StreamKind, delta: &str) -> io::Result<()> {
        if delta.is_empty() {
            return Ok(());
        }
        if self.stream != Some(kind) {
            self.close_stream()?;
            if kind == StreamKind::Reasoning {
                let heading = "-- reasoning --";
                write_line(
                    &mut io::stdout().lock(),
                    heading,
                    "\x1b[2m",
                    self.stdout_styled,
                )?;
            }
            self.stream = Some(kind);
        }
        let color = match kind {
            StreamKind::Assistant => "\x1b[32m",
            StreamKind::Reasoning => "\x1b[2m",
        };
        let mut out = io::stdout().lock();
        if self.stdout_styled {
            write!(out, "{color}{delta}\x1b[0m")
        } else {
            write!(out, "{delta}")
        }?;
        out.flush()
    }

    fn close_stream(&mut self) -> io::Result<()> {
        if self.stream.take().is_some() {
            writeln!(io::stdout().lock())?;
        }
        Ok(())
    }

    fn record_output_result(&mut self, result: io::Result<()>) {
        if let Err(error) = result {
            self.record_output_error(error);
        }
    }

    fn record_output_error(&mut self, error: io::Error) {
        self.job_cancel.cancel();
        if self.output_error.is_none() {
            self.output_error = Some(error);
        }
    }
}

impl Default for PlainUi {
    fn default() -> Self {
        Self::new()
    }
}

impl UserInterface for PlainUi {
    fn emit(&mut self, event: OutputEvent) {
        let result = match event {
            OutputEvent::Header(text) => self.write_message(MessageKind::Info, &text),
            OutputEvent::Message { kind, text } => self.write_message(kind, &text),
            OutputEvent::Stream { kind, delta } => self.write_stream(kind, &delta),
            OutputEvent::StreamFinished => self.close_stream(),
            OutputEvent::ToolStarted { name, action } => {
                self.write_message(MessageKind::Tool, &format!("↳ {name}: {action}"))
            }
            OutputEvent::ToolFinished { name, summary } => {
                self.write_message(MessageKind::Tool, &format!("└ {name}: {summary}"))
            }
        };
        self.record_output_result(result);
    }

    fn start_activity(&mut self, _label: &str) -> CancellationToken {
        self.job_cancel.clone()
    }

    fn stop_activity(&mut self) {}

    fn approve(&mut self, prompt: &str) -> ApprovalChoice {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return ApprovalChoice::No;
        }
        if let Err(error) = self.close_stream().and_then(|()| {
            let mut stdout = io::stdout().lock();
            write!(stdout, "{prompt} [y/N/Esc] ")?;
            stdout.flush()
        }) {
            self.record_output_error(error);
            return ApprovalChoice::No;
        }
        let choice = read_approval().unwrap_or(ApprovalChoice::No);
        let newline = writeln!(io::stdout().lock());
        self.record_output_result(newline);
        choice
    }
}

/// Plain prompt helper, generic for deterministic EOF tests.
pub(crate) fn read_prompt_from<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    prompt: &str,
) -> io::Result<String> {
    write!(output, "{prompt}")?;
    output.flush()?;
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
    }
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

fn write_line(out: &mut dyn Write, text: &str, color: &str, styled: bool) -> io::Result<()> {
    if styled {
        writeln!(out, "{color}{text}\x1b[0m")
    } else {
        writeln!(out, "{text}")
    }
}

fn read_approval() -> io::Result<ApprovalChoice> {
    enable_raw_mode()?;
    let _guard = RawModeGuard;
    let choice = loop {
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('y' | 'Y') => break ApprovalChoice::Yes,
                KeyCode::Char('n' | 'N') | KeyCode::Enter => break ApprovalChoice::No,
                KeyCode::Esc => break ApprovalChoice::Esc,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    break ApprovalChoice::Esc;
                }
                _ => {}
            }
        }
    };
    Ok(choice)
}

struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct BrokenPipe;

    impl Write for BrokenPipe {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    #[test]
    fn plain_prompt_trims_newline() {
        let mut input = "hello\r\n".as_bytes();
        let mut output = Vec::new();
        let line = read_prompt_from(&mut input, &mut output, "> ").unwrap();
        assert_eq!(line, "hello");
        assert_eq!(output, b"> ");
    }

    #[test]
    fn plain_prompt_reports_eof() {
        let mut input = "".as_bytes();
        let mut output = Vec::new();
        let err = read_prompt_from(&mut input, &mut output, "> ").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn line_writer_reports_broken_pipe() {
        let error = write_line(&mut BrokenPipe, "lost", "", false).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn output_error_cancels_work_and_fails_next_prompt() {
        let mut ui = PlainUi::new();
        let cancel = ui.start_activity("thinking");
        ui.record_output_error(io::Error::from(io::ErrorKind::BrokenPipe));

        assert!(cancel.is_cancelled());
        let error = ui.read_prompt("> ").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }
}
