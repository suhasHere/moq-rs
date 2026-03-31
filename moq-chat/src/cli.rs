use clap::Parser;
use std::net;
use url::Url;

#[derive(Parser, Clone)]
#[command(name = "moq-chat")]
#[command(about = "Chat over MoQ - demonstrates namespace subscriptions for multi-user communication")]
pub struct Cli {
    /// Listen for UDP packets on the given address.
    #[arg(long, default_value = "[::]:0")]
    pub bind: net::SocketAddr,

    /// Connect to the given URL starting with https://
    #[arg(short, long, default_value = "https://localhost:4443")]
    pub server: Url,

    /// The TLS configuration.
    #[command(flatten)]
    pub tls: moq_native_ietf::tls::Args,

    /// Chat session/room identifier (required).
    #[arg(short = 'r', long)]
    pub session: String,

    /// Username to display in chat.
    #[arg(short, long)]
    pub username: Option<String>,

    /// Use datagrams instead of streams for messages.
    #[arg(long)]
    pub datagrams: bool,
}

impl Cli {
    /// Returns the chat namespace for this session.
    pub fn chat_namespace(&self) -> String {
        format!("chat/{}", self.session)
    }

    /// Returns the user's publish track path.
    pub fn user_track(&self, user_id: &str) -> String {
        format!("{}/text", user_id)
    }

    /// Returns the full namespace for a user's messages.
    pub fn user_namespace(&self, user_id: &str) -> String {
        format!("chat/{}/{}", self.session, user_id)
    }
}
