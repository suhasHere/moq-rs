//! DTS (Dynamic Track Switching) simulation test.
//!
//! Tests ABR track selection based on bandwidth changes, without network.
//! This validates the DTS tracker and service logic.

use crate::Args;
use anyhow::Result;
use moq_relay_ietf::{DtsService, DtsServiceConfig, DtsTrackerConfig};
use moq_transport::coding::{KeyValuePairs, TrackNamespace};
use moq_transport::message::{DtsParams, SwitchingSetAssignment};
use tracing::info;

fn ns(path: &str) -> TrackNamespace {
    TrackNamespace::from_utf8_path(path)
}

fn make_params(set_id: u64, throughput_kbps: u64, fraction: u64, rank: u8, activate: bool) -> KeyValuePairs {
    let mut params = KeyValuePairs::new();
    let assignment = SwitchingSetAssignment::new(set_id, throughput_kbps)
        .with_fraction(fraction)
        .with_rank(rank)
        .with_activate(activate);
    params.set_switching_set_assignment(&assignment);
    params
}

/// Single switching set ABR scenario
async fn test_single_set_abr(service: &DtsService) -> Result<(u32, u32)> {
    info!("Test: Single Switching Set ABR");
    info!("-------------------------------");

    let session_id = 100;
    let mut passed = 0u32;
    let mut failed = 0u32;

    service.register_subscriber(session_id);

    // Register 3 quality levels
    let params_1080p = make_params(1, 4000, 10, 1, false);
    let params_720p = make_params(1, 2000, 10, 1, false);
    let params_480p = make_params(1, 800, 10, 1, true);

    service.register_track_from_subscribe(session_id, 1, &ns("video"), "1080p", &params_1080p);
    service.register_track_from_subscribe(session_id, 2, &ns("video"), "720p", &params_720p);
    service.register_track_from_subscribe(session_id, 3, &ns("video"), "480p", &params_480p);

    // Test 1: High bandwidth -> 1080p
    info!("  Bandwidth: 5000 kbps");
    service.set_bandwidth(session_id, 5000);
    let selections = service.get_selections(session_id);

    if selections.len() == 1 && selections[0].track.track_name == "1080p" {
        info!("    ✓ Selected: 1080p (correct)");
        passed += 1;
    } else {
        info!("    ✗ Selected: {:?} (expected 1080p)", selections);
        failed += 1;
    }

    // Test 2: Medium bandwidth -> 720p
    info!("  Bandwidth: 2500 kbps");
    service.set_bandwidth(session_id, 2500);
    let selections = service.get_selections(session_id);

    if selections.len() == 1 && selections[0].track.track_name == "720p" {
        info!("    ✓ Selected: 720p (correct)");
        passed += 1;
    } else {
        info!("    ✗ Selected: {:?} (expected 720p)", selections);
        failed += 1;
    }

    // Test 3: Low bandwidth -> 480p
    info!("  Bandwidth: 1000 kbps");
    service.set_bandwidth(session_id, 1000);
    let selections = service.get_selections(session_id);

    if selections.len() == 1 && selections[0].track.track_name == "480p" {
        info!("    ✓ Selected: 480p (correct)");
        passed += 1;
    } else {
        info!("    ✗ Selected: {:?} (expected 480p)", selections);
        failed += 1;
    }

    // Test 4: Very low bandwidth (below lowest track) -> nothing selected
    info!("  Bandwidth: 500 kbps (below 480p threshold of 800)");
    service.set_bandwidth(session_id, 500);
    let selections = service.get_selections(session_id);

    if selections.is_empty() {
        info!("    ✓ Selected: none (correct - below all thresholds)");
        passed += 1;
    } else {
        info!("    ✗ Selected: {:?} (expected none)", selections);
        failed += 1;
    }

    // Test 5: Zero bandwidth -> nothing selected
    info!("  Bandwidth: 0 kbps");
    service.set_bandwidth(session_id, 0);
    let selections = service.get_selections(session_id);

    if selections.is_empty() {
        info!("    ✓ Selected: none (correct - no bandwidth)");
        passed += 1;
    } else {
        info!("    ✗ Selected: {:?} (expected none)", selections);
        failed += 1;
    }

    service.remove_subscriber(session_id);
    info!("");

    Ok((passed, failed))
}

/// Multiple switching sets with priority (rank)
async fn test_multiple_sets_with_rank(service: &DtsService) -> Result<(u32, u32)> {
    info!("Test: Multiple Switching Sets with Rank Priority");
    info!("-------------------------------------------------");

    let session_id = 200;
    let mut passed = 0u32;
    let mut failed = 0u32;

    service.register_subscriber(session_id);

    // Set 1: Main video (rank=1, fraction=6) - higher priority
    let main_1080p = make_params(1, 3000, 6, 1, false);
    let main_720p = make_params(1, 1500, 6, 1, false);
    let main_480p = make_params(1, 800, 6, 1, true);

    service.register_track_from_subscribe(session_id, 10, &ns("video/main"), "1080p", &main_1080p);
    service.register_track_from_subscribe(session_id, 11, &ns("video/main"), "720p", &main_720p);
    service.register_track_from_subscribe(session_id, 12, &ns("video/main"), "480p", &main_480p);

    // Set 2: Secondary video (rank=2, fraction=4) - lower priority
    let sec_720p = make_params(2, 1500, 4, 2, false);
    let sec_480p = make_params(2, 800, 4, 2, true);

    service.register_track_from_subscribe(session_id, 20, &ns("video/secondary"), "720p", &sec_720p);
    service.register_track_from_subscribe(session_id, 21, &ns("video/secondary"), "480p", &sec_480p);

    // Test 1: Plenty of bandwidth for both
    // sum_f = 10, main target = 5000*6/10 = 3000, secondary target = 5000*4/10 = 2000
    info!("  Bandwidth: 5000 kbps");
    service.set_bandwidth(session_id, 5000);
    let selections = service.get_selections(session_id);

    let main_track = selections.iter().find(|s| s.set_id == 1);
    let sec_track = selections.iter().find(|s| s.set_id == 2);

    if selections.len() == 2
        && main_track.map(|s| s.track.track_name.as_str()) == Some("1080p")
        && sec_track.map(|s| s.track.track_name.as_str()) == Some("720p")
    {
        info!("    ✓ Main=1080p, Secondary=720p (correct)");
        passed += 1;
    } else {
        info!("    ✗ Selections: {:?} (expected Main=1080p, Secondary=720p)", selections);
        failed += 1;
    }

    // Test 2: Constrained bandwidth - main protected
    // sum_f = 10, main target = 3000*6/10 = 1800, secondary target = 3000*4/10 = 1200
    // But rank-based: main (rank=1) gets first allocation
    info!("  Bandwidth: 3000 kbps");
    service.set_bandwidth(session_id, 3000);
    let selections = service.get_selections(session_id);

    let main_track = selections.iter().find(|s| s.set_id == 1);
    let sec_track = selections.iter().find(|s| s.set_id == 2);

    // Main should still get 1080p or at least 720p, secondary should degrade more
    if selections.len() == 2 {
        info!("    Main={:?}, Secondary={:?}",
            main_track.map(|s| s.track.track_name.as_str()),
            sec_track.map(|s| s.track.track_name.as_str())
        );
        // Main should be at least 720p, secondary at most 480p
        let main_ok = main_track.map(|s| s.track.throughput_kbps >= 1500).unwrap_or(false);
        let sec_ok = sec_track.is_some();
        if main_ok && sec_ok {
            info!("    ✓ Main protected (>= 720p), secondary present");
            passed += 1;
        } else {
            info!("    ✗ Unexpected degradation");
            failed += 1;
        }
    } else {
        info!("    ✗ Expected 2 selections, got {}", selections.len());
        failed += 1;
    }

    // Test 3: Very constrained - secondary may be dropped
    info!("  Bandwidth: 1500 kbps");
    service.set_bandwidth(session_id, 1500);
    let selections = service.get_selections(session_id);

    let main_track = selections.iter().find(|s| s.set_id == 1);

    if main_track.is_some() {
        info!("    ✓ Main still selected: {:?}", main_track.map(|s| &s.track.track_name));
        passed += 1;
    } else {
        info!("    ✗ Main not selected");
        failed += 1;
    }

    service.remove_subscriber(session_id);
    info!("");

    Ok((passed, failed))
}

/// Object forwarding with group-boundary switching
async fn test_group_boundary_switching(service: &DtsService) -> Result<(u32, u32)> {
    info!("Test: Group-Boundary Switching");
    info!("------------------------------");

    let session_id = 300;
    let mut passed = 0u32;
    let mut failed = 0u32;

    service.register_subscriber(session_id);

    // Two quality levels
    let high = make_params(1, 2000, 10, 1, false);
    let low = make_params(1, 500, 10, 1, true);

    service.register_track_from_subscribe(session_id, 1, &ns("video"), "high", &high);
    service.register_track_from_subscribe(session_id, 2, &ns("video"), "low", &low);

    // Start with high bandwidth
    service.set_bandwidth(session_id, 5000);

    // Group 1: High quality objects
    info!("  Group 1 (bandwidth=5000): High quality selected");
    let fwd = service.should_forward_object(session_id, 1, 1, 0);
    if fwd {
        info!("    ✓ High track, group 1, obj 0: forwarded");
        passed += 1;
    } else {
        info!("    ✗ High track should be forwarded");
        failed += 1;
    }

    // Low track should not forward
    let fwd = service.should_forward_object(session_id, 2, 1, 0);
    if !fwd {
        info!("    ✓ Low track, group 1, obj 0: not forwarded");
        passed += 1;
    } else {
        info!("    ✗ Low track should not be forwarded");
        failed += 1;
    }

    // Reduce bandwidth mid-group
    info!("  Bandwidth reduced to 1000 kbps mid-group");
    service.set_bandwidth(session_id, 1000);

    // Continue high track in same group (should still forward until group boundary)
    let fwd = service.should_forward_object(session_id, 1, 1, 1);
    if fwd {
        info!("    ✓ High track continues in same group");
        passed += 1;
    } else {
        info!("    ✗ High track should continue until group boundary");
        failed += 1;
    }

    // New group from low track - switch should happen
    info!("  Group 2: Low quality should take over");
    let fwd = service.should_forward_object(session_id, 2, 2, 0);
    if fwd {
        info!("    ✓ Low track, group 2, obj 0: forwarded (switch complete)");
        passed += 1;
    } else {
        info!("    ✗ Low track should be forwarded in new group");
        failed += 1;
    }

    // High track group 2 should not forward
    let fwd = service.should_forward_object(session_id, 1, 2, 0);
    if !fwd {
        info!("    ✓ High track, group 2: not forwarded");
        passed += 1;
    } else {
        info!("    ✗ High track should not forward after switch");
        failed += 1;
    }

    service.remove_subscriber(session_id);
    info!("");

    Ok((passed, failed))
}

/// Bandwidth estimation simulation
async fn test_bandwidth_dynamics(service: &DtsService) -> Result<(u32, u32)> {
    info!("Test: Bandwidth Dynamics (Simulated)");
    info!("------------------------------------");

    let session_id = 400;
    let mut passed = 0u32;
    let mut failed = 0u32;

    service.register_subscriber(session_id);

    // Single quality track to focus on bandwidth behavior
    let params = make_params(1, 1000, 10, 1, true);
    service.register_track_from_subscribe(session_id, 1, &ns("video"), "track", &params);

    // Simulate bandwidth fluctuations
    let bandwidth_samples = vec![
        (5000, true),  // High - selected
        (4000, true),  // Still high
        (2000, true),  // Medium - selected
        (800, false),  // Low - not enough
        (1200, true),  // Above threshold - selected
        (1000, true),  // At threshold - selected
        (999, false),  // Below threshold
    ];

    info!("  Testing bandwidth threshold behavior:");
    for (bw, expected_selected) in bandwidth_samples {
        service.set_bandwidth(session_id, bw);
        let selected = service.is_track_selected(session_id, 1);

        if selected == expected_selected {
            info!("    ✓ {} kbps: selected={} (correct)", bw, selected);
            passed += 1;
        } else {
            info!("    ✗ {} kbps: selected={} (expected {})", bw, selected, expected_selected);
            failed += 1;
        }
    }

    service.remove_subscriber(session_id);
    info!("");

    Ok((passed, failed))
}

pub async fn run(args: Args) -> Result<()> {
    info!("DTS Simulation Test");
    info!("===================");
    info!("");

    // Create DTS service with test configuration
    let config = DtsServiceConfig {
        tracker: DtsTrackerConfig {
            min_switch_interval_ms: 0,  // No hysteresis for testing
            upswitch_headroom_pct: 0,   // No headroom for predictable results
            enable_logging: args.verbose,
        },
        enable_logging: args.verbose,
        ..Default::default()
    };

    let service = DtsService::with_config(config);

    let mut total_passed = 0u32;
    let mut total_failed = 0u32;

    // Run test suites
    let (p, f) = test_single_set_abr(&service).await?;
    total_passed += p;
    total_failed += f;

    let (p, f) = test_multiple_sets_with_rank(&service).await?;
    total_passed += p;
    total_failed += f;

    let (p, f) = test_group_boundary_switching(&service).await?;
    total_passed += p;
    total_failed += f;

    let (p, f) = test_bandwidth_dynamics(&service).await?;
    total_passed += p;
    total_failed += f;

    // Summary
    info!("Results Summary");
    info!("===============");
    info!("  Passed: {}", total_passed);
    info!("  Failed: {}", total_failed);
    info!("");

    if total_failed == 0 {
        info!("DTS_TEST_RESULT: SUCCESS");
        Ok(())
    } else {
        info!("DTS_TEST_RESULT: FAILURE");
        std::process::exit(1);
    }
}
