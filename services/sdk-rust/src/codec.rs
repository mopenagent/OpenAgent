use crate::error::Result;
use crate::types::{Frame, OutboundEvent};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// Reads newline-delimited JSON frames from any async reader (Unix socket or TCP).
#[derive(Debug)]
pub struct Decoder<R: AsyncRead + Unpin> {
    reader: BufReader<R>,
}

impl<R: AsyncRead + Unpin> Decoder<R> {
    pub fn new(read_half: R) -> Self {
        Self {
            reader: BufReader::new(read_half),
        }
    }

    /// Read the next MCP-lite frame.
    ///
    /// Returns `Ok(None)` on EOF (connection closed cleanly).
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Io`] if the read fails, or
    /// [`crate::Error::Codec`] if the line is not valid JSON.
    pub async fn next_frame(&mut self) -> Result<Option<Frame>> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await?;

        if n == 0 {
            return Ok(None); // EOF
        }

        let line = line.trim();
        if line.is_empty() {
            return Ok(None);
        }

        let frame: Frame = serde_json::from_str(line)?;
        Ok(Some(frame))
    }
}

/// Writes newline-delimited JSON frames to any async writer (Unix socket or TCP).
#[derive(Debug)]
pub struct Encoder<W: AsyncWrite + Unpin> {
    writer: W,
}

impl<W: AsyncWrite + Unpin> Encoder<W> {
    pub fn new(write_half: W) -> Self {
        Self { writer: write_half }
    }

    /// Serialize and write a frame followed by a newline.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Codec`] if serialization fails, or
    /// [`crate::Error::Io`] if the write or flush fails.
    pub async fn write_frame(&mut self, frame: &Frame) -> Result<()> {
        let mut data = serde_json::to_vec(frame)?;
        data.push(b'\n');
        self.writer.write_all(&data).await?;
        self.writer.flush().await?;
        Ok(())
    }

    /// Serialize and write an event frame followed by a newline.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Codec`] if serialization fails, or
    /// [`crate::Error::Io`] if the write or flush fails.
    pub async fn write_event(&mut self, event: &OutboundEvent) -> Result<()> {
        let mut data = serde_json::to_vec(event)?;
        data.push(b'\n');
        self.writer.write_all(&data).await?;
        self.writer.flush().await?;
        Ok(())
    }
}
