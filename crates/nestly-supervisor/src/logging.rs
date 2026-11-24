use std::io::{self, Write};
use std::sync::Arc;

use nestly_core::Redactor;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

/// Build a tracing subscriber whose writer redacts secrets line by line.
pub fn init(level: &str, redactor: Arc<Redactor>) {
    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));
    let writer = RedactingMakeWriter { redactor };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(writer)
        .try_init();
}

#[derive(Clone)]
struct RedactingMakeWriter {
    redactor: Arc<Redactor>,
}

impl<'a> MakeWriter<'a> for RedactingMakeWriter {
    type Writer = RedactingWriter<io::Stdout>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter {
            inner: io::stdout(),
            redactor: self.redactor.clone(),
            buffer: Vec::with_capacity(4096),
        }
    }
}

struct RedactingWriter<W: Write> {
    inner: W,
    redactor: Arc<Redactor>,
    buffer: Vec<u8>,
}

impl<W: Write> RedactingWriter<W> {
    fn flush_complete_lines(&mut self) -> io::Result<()> {
        while let Some(position) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=position).collect();
            self.write_redacted(&line)?;
        }
        if self.buffer.len() > 64 * 1024 {
            let line = std::mem::take(&mut self.buffer);
            self.write_redacted(&line)?;
        }
        Ok(())
    }

    fn write_redacted(&mut self, bytes: &[u8]) -> io::Result<()> {
        let text = String::from_utf8_lossy(bytes);
        let redacted = self.redactor.redact(&text);
        self.inner.write_all(redacted.as_bytes())
    }
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        self.flush_complete_lines()?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            self.write_redacted(&line)?;
        }
        self.inner.flush()
    }
}

impl<W: Write> Drop for RedactingWriter<W> {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::Arc;

    use nestly_core::Redactor;

    use super::RedactingWriter;

    #[test]
    fn redacts_complete_lines() {
        let redactor = Arc::new(Redactor::new(["super-secret-123"]));
        let mut writer = RedactingWriter {
            inner: Vec::new(),
            redactor,
            buffer: Vec::new(),
        };
        writer.write_all(b"hello super-secret-123\n").unwrap();
        writer.flush().unwrap();
        let output = String::from_utf8(std::mem::take(&mut writer.inner)).unwrap();
        assert!(!output.contains("super-secret-123"));
        assert!(output.contains("***"));
    }

    #[test]
    fn redacts_secret_split_across_writes() {
        let redactor = Arc::new(Redactor::new(["super-secret-123"]));
        let mut writer = RedactingWriter {
            inner: Vec::new(),
            redactor,
            buffer: Vec::new(),
        };
        writer.write_all(b"hello super-sec").unwrap();
        writer.write_all(b"ret-123\n").unwrap();
        writer.flush().unwrap();
        let output = String::from_utf8(std::mem::take(&mut writer.inner)).unwrap();
        assert!(!output.contains("super-secret-123"));
    }
}
