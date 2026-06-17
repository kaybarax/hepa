//! Benchmark harness: aggregate timing-record medians and compare against the
//! Phase 0.4 HOCA reference baseline. All output is sanitized.

use crate::contracts::HepaTimingRecord;

/// A HOCA reference median for one canonical benchmark task, taken from the
/// Phase 0.4 baseline (sanitized — no local paths or private names).
#[derive(Debug, Clone, PartialEq)]
pub struct HepaBenchReference {
    pub benchmark_id: String,
    pub median_wall_seconds: f64,
    pub median_agent_loops: u32,
    pub median_containers: u32,
    pub median_install_events: u32,
    pub median_peak_rss_mib: f64,
}

/// The Phase 0.4 HOCA v1.1.0 reference medians.
pub fn hoca_reference_medians() -> Vec<HepaBenchReference> {
    vec![
        HepaBenchReference {
            benchmark_id: "PB-DOC-001".to_string(),
            median_wall_seconds: 80.51,
            median_agent_loops: 2,
            median_containers: 1,
            median_install_events: 1,
            median_peak_rss_mib: 147.21,
        },
        HepaBenchReference {
            benchmark_id: "PB-FE-001".to_string(),
            median_wall_seconds: 251.33,
            median_agent_loops: 1,
            median_containers: 1,
            median_install_events: 1,
            median_peak_rss_mib: 147.01,
        },
    ]
}

pub fn reference_for(benchmark_id: &str) -> Option<HepaBenchReference> {
    hoca_reference_medians()
        .into_iter()
        .find(|reference| reference.benchmark_id == benchmark_id)
}

/// Aggregated HEPA candidate measurements for one benchmark.
#[derive(Debug, Clone, PartialEq)]
pub struct HepaBenchCandidate {
    pub benchmark_id: String,
    pub wall_seconds: f64,
    pub agent_loops: u32,
    pub manager_passes: u32,
    pub containers: u32,
    pub install_events: u32,
    pub peak_rss_mib: f64,
    pub board_sync_overhead_seconds: f64,
    pub runs: usize,
    pub manual_timing: bool,
}

/// Aggregate one benchmark's repeated timing records into median measurements.
/// `manual_timing` flags timings captured manually until automated capture
/// lands.
pub fn aggregate_timing_records(
    benchmark_id: &str,
    records: &[HepaTimingRecord],
    peak_rss_mib: f64,
    board_sync_overhead_seconds: f64,
    manual_timing: bool,
) -> Result<HepaBenchCandidate, String> {
    if records.is_empty() {
        return Err("benchmark requires at least one timing record".to_string());
    }
    let walls: Vec<f64> = records.iter().map(record_wall_seconds).collect();
    let agent_loops: Vec<u32> = records.iter().map(|r| r.counters.agent_loops).collect();
    let manager_passes: Vec<u32> = records.iter().map(|r| r.counters.manager_passes).collect();
    let containers: Vec<u32> = records.iter().map(|r| r.counters.container_count).collect();
    let installs: Vec<u32> = records.iter().map(|r| r.counters.install_events).collect();

    Ok(HepaBenchCandidate {
        benchmark_id: benchmark_id.to_string(),
        wall_seconds: median_f64(&walls),
        agent_loops: median_u32(&agent_loops),
        manager_passes: median_u32(&manager_passes),
        containers: median_u32(&containers),
        install_events: median_u32(&installs),
        peak_rss_mib,
        board_sync_overhead_seconds,
        runs: records.len(),
        manual_timing,
    })
}

fn record_wall_seconds(record: &HepaTimingRecord) -> f64 {
    record
        .phases
        .iter()
        .map(|phase| phase.duration_seconds)
        .sum()
}

fn median_f64(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[mid]
    }
}

fn median_u32(values: &[u32]) -> u32 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted[sorted.len() / 2]
}

/// Whether the architecture targets are met by a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaBenchTargets {
    pub one_loop_per_attempt: bool,
    pub zero_default_containers: bool,
    pub manager_passes_within_budget: bool,
    pub faster_than_reference: bool,
}

impl HepaBenchTargets {
    pub fn all_met(&self) -> bool {
        self.one_loop_per_attempt
            && self.zero_default_containers
            && self.manager_passes_within_budget
            && self.faster_than_reference
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HepaBenchComparison {
    pub benchmark_id: String,
    pub reference: HepaBenchReference,
    pub candidate: HepaBenchCandidate,
    pub wall_delta_seconds: f64,
    pub wall_delta_percent: f64,
    pub targets: HepaBenchTargets,
}

/// Compare a candidate against the HOCA reference, computing deltas and the
/// architecture target check. Compares only identical benchmark ids.
pub fn compare_to_reference(candidate: &HepaBenchCandidate) -> Result<HepaBenchComparison, String> {
    let reference = reference_for(&candidate.benchmark_id)
        .ok_or_else(|| format!("no HOCA reference for benchmark {}", candidate.benchmark_id))?;
    let wall_delta_seconds = candidate.wall_seconds - reference.median_wall_seconds;
    let wall_delta_percent = if reference.median_wall_seconds > 0.0 {
        wall_delta_seconds / reference.median_wall_seconds * 100.0
    } else {
        0.0
    };
    let targets = HepaBenchTargets {
        one_loop_per_attempt: candidate.agent_loops <= 1,
        zero_default_containers: candidate.containers == 0,
        manager_passes_within_budget: candidate.manager_passes <= 2,
        faster_than_reference: candidate.wall_seconds <= reference.median_wall_seconds,
    };
    Ok(HepaBenchComparison {
        benchmark_id: candidate.benchmark_id.clone(),
        reference,
        candidate: candidate.clone(),
        wall_delta_seconds,
        wall_delta_percent,
        targets,
    })
}

/// Render a sanitized markdown comparison table.
pub fn render_comparison_table(comparison: &HepaBenchComparison) -> String {
    let candidate = &comparison.candidate;
    let reference = &comparison.reference;
    let label = if candidate.manual_timing {
        " (manual timing)"
    } else {
        ""
    };
    format!(
        "### Benchmark {id}{label}\n\
         | Metric | HOCA reference | HEPA candidate | Delta |\n\
         | --- | ---: | ---: | ---: |\n\
         | Wall time (s) | {ref_wall:.2} | {cand_wall:.2} | {dwall:.2} ({dpct:.1}%) |\n\
         | Agent loops | {ref_loops} | {cand_loops} | {dloops} |\n\
         | Manager passes | n/a | {cand_mgr} | n/a |\n\
         | Containers | {ref_ctr} | {cand_ctr} | {dctr} |\n\
         | Install events | {ref_inst} | {cand_inst} | {dinst} |\n\
         | Board-sync overhead (s) | n/a | {board:.2} | n/a |\n\
         | Runs (median of) | 2 | {runs} | n/a |\n\
         | Targets met | n/a | {targets} | n/a |\n",
        id = comparison.benchmark_id,
        label = label,
        ref_wall = reference.median_wall_seconds,
        cand_wall = candidate.wall_seconds,
        dwall = comparison.wall_delta_seconds,
        dpct = comparison.wall_delta_percent,
        ref_loops = reference.median_agent_loops,
        cand_loops = candidate.agent_loops,
        dloops = candidate.agent_loops as i64 - reference.median_agent_loops as i64,
        cand_mgr = candidate.manager_passes,
        ref_ctr = reference.median_containers,
        cand_ctr = candidate.containers,
        dctr = candidate.containers as i64 - reference.median_containers as i64,
        ref_inst = reference.median_install_events,
        cand_inst = candidate.install_events,
        dinst = candidate.install_events as i64 - reference.median_install_events as i64,
        board = candidate.board_sync_overhead_seconds,
        runs = candidate.runs,
        targets = comparison.targets.all_met(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{
        CONTRACT_SCHEMA_VERSION, HepaAgentRole, HepaPhaseStatus, HepaTimingCounters,
        HepaTimingPhase,
    };

    fn timing(wall: f64, loops: u32, manager: u32, containers: u32) -> HepaTimingRecord {
        HepaTimingRecord {
            schema_version: CONTRACT_SCHEMA_VERSION,
            run_id: "run-1".to_string(),
            phases: vec![HepaTimingPhase {
                name: "worker".to_string(),
                status: HepaPhaseStatus::Completed,
                duration_seconds: wall,
                round: Some(1),
                role: Some(HepaAgentRole::Worker),
                adapter_id: Some("fake".to_string()),
                routing_reason: Some("default".to_string()),
                sandbox_posture: Some("host-worktree".to_string()),
            }],
            counters: HepaTimingCounters {
                agent_loops: loops,
                manager_passes: manager,
                worker_profile_llm_calls: 0,
                reviewer_passes: 1,
                install_events: 0,
                container_count: containers,
            },
        }
    }

    #[test]
    fn aggregates_medians_across_runs() {
        let records = vec![timing(10.0, 1, 1, 0), timing(20.0, 1, 1, 0)];
        let candidate =
            aggregate_timing_records("PB-DOC-001", &records, 50.0, 0.1, false).expect("aggregate");
        assert_eq!(candidate.wall_seconds, 15.0);
        assert_eq!(candidate.agent_loops, 1);
        assert_eq!(candidate.runs, 2);
    }

    #[test]
    fn compares_only_identical_benchmark_ids() {
        let candidate =
            aggregate_timing_records("UNKNOWN", &[timing(5.0, 1, 1, 0)], 0.0, 0.0, false)
                .expect("aggregate");
        assert!(compare_to_reference(&candidate).is_err());
    }

    #[test]
    fn hepa_candidate_meets_targets_and_beats_reference() {
        let candidate =
            aggregate_timing_records("PB-DOC-001", &[timing(2.0, 1, 1, 0)], 40.0, 0.05, false)
                .expect("aggregate");
        let comparison = compare_to_reference(&candidate).expect("compare");

        // 1 loop, 0 containers, <=2 manager passes, faster than 80.51s.
        assert!(comparison.targets.all_met(), "{:?}", comparison.targets);
        assert!(comparison.wall_delta_seconds < 0.0);

        let table = render_comparison_table(&comparison);
        assert!(table.contains("Benchmark PB-DOC-001"));
        assert!(table.contains("HOCA reference"));
        // Sanitized: no local paths or private names.
        assert!(!table.contains("/Users/"));
    }

    #[test]
    fn manual_timing_is_clearly_labeled() {
        let candidate =
            aggregate_timing_records("PB-DOC-001", &[timing(2.0, 1, 1, 0)], 40.0, 0.0, true)
                .expect("aggregate");
        let table = render_comparison_table(&compare_to_reference(&candidate).expect("compare"));
        assert!(table.contains("(manual timing)"));
    }
}
