//! Chat module for MoQ-based multi-user communication.
//!
//! Implements the chat protocol similar to quicr-go/examples/chat:
//! - Subscribe to namespace `chat/<session>` to discover users
//! - Each user publishes to `chat/<session>/<user-id>/text`
//! - Messages are timestamped and sequenced

use anyhow::{Context, Result};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use moq_transport::serve::{
    DatagramsWriter, SubgroupsWriter, TrackReader, TrackReaderMode,
    Datagram as ServeDatagram,
};
use moq_transport::data::ExtensionHeaders;
use std::io::{self, BufRead, Write};
use tokio::sync::mpsc;

/// A chat message with metadata.
#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub timestamp: DateTime<Utc>,
    pub sequence: u64,
    pub user_id: String,
    pub username: String,
    pub content: String,
}

impl ChatMessage {
    pub fn new(user_id: &str, username: &str, content: String) -> Self {
        Self {
            timestamp: Utc::now(),
            sequence: 0,
            user_id: user_id.to_string(),
            username: username.to_string(),
            content,
        }
    }

    /// Encode message for transmission.
    pub fn encode(&self) -> Bytes {
        let formatted = format!(
            "[{}:{}] {}\n",
            self.timestamp.timestamp(),
            self.sequence,
            self.content
        );
        Bytes::from(formatted)
    }

    /// Decode message from bytes.
    pub fn decode(user_id: &str, username: &str, data: &[u8]) -> Option<Self> {
        let text = String::from_utf8_lossy(data);

        // Parse format: [timestamp:sequence] content
        if let Some(rest) = text.strip_prefix('[') {
            if let Some((meta, content)) = rest.split_once("] ") {
                if let Some((ts_str, seq_str)) = meta.split_once(':') {
                    let timestamp = ts_str.parse::<i64>().ok()?;
                    let sequence = seq_str.parse::<u64>().ok()?;

                    return Some(Self {
                        timestamp: DateTime::from_timestamp(timestamp, 0)?,
                        sequence,
                        user_id: user_id.to_string(),
                        username: username.to_string(),
                        content: content.trim().to_string(),
                    });
                }
            }
        }

        // Fallback: treat entire content as message
        Some(Self {
            timestamp: Utc::now(),
            sequence: 0,
            user_id: user_id.to_string(),
            username: username.to_string(),
            content: text.trim().to_string(),
        })
    }

    /// Format for display.
    pub fn display(&self) -> String {
        let time = self.timestamp.format("%H:%M:%S");
        format!("[{}] {}: {}", time, self.username, self.content)
    }
}

/// Publisher for chat messages using subgroups (streams).
pub struct StreamPublisher {
    writer: SubgroupsWriter,
    user_id: String,
    username: String,
    sequence: u64,
}

impl StreamPublisher {
    pub fn new(writer: SubgroupsWriter, user_id: String, username: String) -> Self {
        Self {
            writer,
            user_id,
            username,
            sequence: 0,
        }
    }

    /// Publish a message.
    pub fn publish(&mut self, content: String) -> Result<()> {
        let mut msg = ChatMessage::new(&self.user_id, &self.username, content);
        msg.sequence = self.sequence;
        self.sequence += 1;

        let data = msg.encode();

        // Create a new subgroup for each message with priority 0
        let mut subgroup = self.writer.append(0)?;

        // Write the message data
        subgroup.write(data)?;

        Ok(())
    }

    /// Run the publisher, reading from stdin.
    pub async fn run(mut self, mut rx: mpsc::Receiver<String>) -> Result<()> {
        while let Some(line) = rx.recv().await {
            if line.trim().is_empty() {
                continue;
            }

            if let Err(e) = self.publish(line) {
                log::error!("failed to publish message: {}", e);
            }
        }

        Ok(())
    }
}

/// Publisher for chat messages using datagrams.
pub struct DatagramPublisher {
    writer: DatagramsWriter,
    user_id: String,
    username: String,
    sequence: u64,
    group_id: u64,
}

impl DatagramPublisher {
    pub fn new(writer: DatagramsWriter, user_id: String, username: String) -> Self {
        Self {
            writer,
            user_id,
            username,
            sequence: 0,
            group_id: 0,
        }
    }

    /// Publish a message.
    pub fn publish(&mut self, content: String) -> Result<()> {
        let mut msg = ChatMessage::new(&self.user_id, &self.username, content);
        msg.sequence = self.sequence;

        let data = msg.encode();

        let datagram = ServeDatagram {
            group_id: self.group_id,
            object_id: self.sequence,
            priority: 0,
            payload: data,
            extension_headers: ExtensionHeaders::default(),
        };

        self.writer.write(datagram)?;

        self.sequence += 1;
        self.group_id += 1;

        Ok(())
    }

    /// Run the publisher, reading from stdin.
    pub async fn run(mut self, mut rx: mpsc::Receiver<String>) -> Result<()> {
        while let Some(line) = rx.recv().await {
            if line.trim().is_empty() {
                continue;
            }

            if let Err(e) = self.publish(line) {
                log::error!("failed to publish message: {}", e);
            }
        }

        Ok(())
    }
}

/// Subscriber for chat messages.
pub struct Subscriber {
    reader: TrackReader,
    user_id: String,
    username: String,
}

impl Subscriber {
    pub fn new(reader: TrackReader, user_id: String, username: String) -> Self {
        Self {
            reader,
            user_id,
            username,
        }
    }

    /// Run the subscriber, printing received messages.
    pub async fn run(mut self) -> Result<()> {
        loop {
            let mode = self.reader.mode().await.context("failed to get track mode")?;

            match mode {
                TrackReaderMode::Subgroups(mut subgroups) => {
                    while let Some(mut subgroup) = subgroups.next().await? {
                        // Read all objects from the subgroup
                        while let Some(data) = subgroup.read_next().await? {
                            if let Some(msg) = ChatMessage::decode(
                                &self.user_id,
                                &self.username,
                                &data,
                            ) {
                                println!("{}", msg.display());
                                io::stdout().flush().ok();
                            }
                        }
                    }
                }
                TrackReaderMode::Datagrams(mut datagrams) => {
                    while let Some(datagram) = datagrams.read().await? {
                        if let Some(msg) = ChatMessage::decode(
                            &self.user_id,
                            &self.username,
                            &datagram.payload,
                        ) {
                            println!("{}", msg.display());
                            io::stdout().flush().ok();
                        }
                    }
                }
                TrackReaderMode::Stream(mut stream) => {
                    // For stream mode, read groups then objects
                    while let Some(mut group) = stream.next().await? {
                        while let Some(data) = group.read_next().await? {
                            if let Some(msg) = ChatMessage::decode(
                                &self.user_id,
                                &self.username,
                                &data,
                            ) {
                                println!("{}", msg.display());
                                io::stdout().flush().ok();
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Read lines from stdin in a blocking task.
pub fn spawn_stdin_reader() -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel(100);

    std::thread::spawn(move || {
        let stdin = io::stdin();
        let handle = stdin.lock();

        for line in handle.lines() {
            match line {
                Ok(text) => {
                    if tx.blocking_send(text).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    rx
}

/// Print welcome message and instructions.
pub fn print_welcome(session: &str, user_id: &str, username: &str) {
    println!("========================================");
    println!("  MoQ Chat - Session: {}", session);
    println!("========================================");
    println!("  User ID:  {}", user_id);
    println!("  Username: {}", username);
    println!("----------------------------------------");
    println!("  Type your message and press Enter.");
    println!("  Press Ctrl+C to exit.");
    println!("========================================");
    println!();
}
