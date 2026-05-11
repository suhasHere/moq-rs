//! End-to-end test driver for MOQ TRACK_FILTER (Top-N) and DTS functionality.
//!
//! Supports multiple modes:
//! - `sim` (default): Simulation mode using TopNTracker directly in-memory
//! - `e2e`: End-to-end mode connecting to a real relay over QUIC/WebTransport
//! - `dts`: DTS (Dynamic Track Switching) simulation test
//!
//! Simulates realistic speech activity patterns with multiple publishers
//! and verifies that subscribers with TRACK_FILTER receive the correct tracks.

mod dts_e2e;
mod dts_sim;
mod e2e;
mod sim;
mod speech;
mod stats;
mod viz;

use clap::{Parser, ValueEnum};
use tracing::info;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TestMode {
    /// Simulation mode - uses TopNTracker directly (no network)
    Sim,
    /// End-to-end mode - connects to a real relay
    E2e,
    /// DTS (Dynamic Track Switching) simulation test
    Dts,
    /// DTS end-to-end mode - connects to a real relay with DTS
    DtsE2e,
}

#[derive(Parser, Clone)]
#[command(name = "moq-topn-test")]
#[command(about = "End-to-end test driver for MOQ Top-N filtering")]
pub struct Args {
    /// Test mode: sim (simulation) or e2e (end-to-end with relay)
    #[arg(short = 'M', long = "test-mode", default_value = "sim")]
    pub test_mode: TestMode,

    /// DTS e2e submode: "subscribe" or "sub-ns-topn"
    #[arg(long, default_value = None)]
    pub mode: Option<String>,

    /// Relay URL for e2e mode (e.g., https://localhost:4443)
    #[arg(short, long, default_value = "https://localhost:4443")]
    pub relay: String,

    /// Number of publishers (X)
    #[arg(short = 'x', long, default_value = "10")]
    pub publishers: usize,

    /// Number of subscribers
    #[arg(short = 'y', long, default_value = "5")]
    pub subscribers: usize,

    /// Top-N filter value for subscribers
    #[arg(short = 'n', long, default_value = "3")]
    pub top_n: u8,

    /// Test duration in seconds
    #[arg(short, long, default_value = "30")]
    pub duration: u64,

    /// Group interval in milliseconds
    #[arg(long, default_value = "2000")]
    pub group_interval_ms: u64,

    /// Namespace for the test
    #[arg(long, default_value = "topn-test")]
    pub namespace: String,

    /// Tie-breaking policy: "oldest" or "recent"
    #[arg(long, default_value = "oldest")]
    pub tie_break: String,

    /// Staleness timeout in seconds (0 = disabled)
    #[arg(long, default_value = "10")]
    pub staleness_timeout: u64,

    /// TLS options (for e2e mode)
    #[command(flatten)]
    pub tls: moq_native_ietf::tls::Args,

    /// Verbose output
    #[arg(short, long)]
    pub verbose: bool,

    /// Output path for timeline visualization SVG
    #[arg(long)]
    pub viz_output: Option<String>,

    /// Disable TOPN_EVENT logging for visualization
    #[arg(long)]
    pub no_topn_log: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Initialize logging
    let filter = if args.verbose {
        "moq_topn_test=debug,moq_transport=debug"
    } else {
        "moq_topn_test=info,moq_transport=warn"
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .init();

    info!("MOQ Top-N Test Driver");
    info!("=====================");
    info!("Mode: {:?}", args.test_mode);
    if args.test_mode == TestMode::E2e || args.test_mode == TestMode::DtsE2e {
        info!("Relay: {}", args.relay);
    }
    if args.test_mode == TestMode::DtsE2e {
        info!("DTS submode: {}", args.mode.as_deref().unwrap_or("subscribe"));
    }
    info!("Publishers (X): {}", args.publishers);
    info!("Subscribers (Y): {}", args.subscribers);
    info!("Top-N filter: {}", args.top_n);
    info!("Duration: {}s", args.duration);
    info!("Group interval: {}ms", args.group_interval_ms);
    info!("Tie-break policy: {}", args.tie_break);
    info!(
        "Staleness timeout: {}",
        if args.staleness_timeout == 0 {
            "disabled".to_string()
        } else {
            format!("{}s", args.staleness_timeout)
        }
    );
    info!("");

    match args.test_mode {
        TestMode::Sim => sim::run(args).await,
        TestMode::E2e => e2e::run(args).await,
        TestMode::Dts => dts_sim::run(args).await,
        TestMode::DtsE2e => dts_e2e::run(args).await,
    }
}
