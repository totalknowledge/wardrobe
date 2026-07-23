use crate::config::LibraryProfile;
use crate::engine::PhaseName;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub struct BenchmarkReport {
    pub profile: LibraryProfile,
    pub run_dir: PathBuf,
    pub targets: Vec<TargetReport>,
}

impl BenchmarkReport {
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# Cross-Engine Performance Benchmark\n\n");
        out.push_str(&format!("- Run directory: `{}`\n", self.run_dir.display()));
        out.push_str(&format!(
            "- Profile: {} entity records, {} book records, chunk size {}, {} traversal queries, {} point lookups, {} range lookups, {} delete-by-ID operations\n\n",
            self.profile.entity_records,
            self.profile.book_records,
            self.profile.chunk_size,
            self.profile.traversal_queries,
            self.profile.point_lookups,
            self.profile.range_lookups,
            self.profile.delete_by_id_operations,
        ));
        out.push_str("| Target | Phase | Operations | Total us | OPS | Mean us | p95 us | p99 us | Storage bytes |\n");
        out.push_str("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
        for target in &self.targets {
            if let Some(reason) = &target.unavailable_reason {
                out.push_str(&format!(
                    "| {} | Unavailable | 0 | 0 | 0.00 | 0.00 | 0.00 | 0.00 | 0 |\n",
                    target.name
                ));
                let _ = reason;
            } else {
                for phase in &target.phases {
                    out.push_str(&format!(
                        "| {} | {} | {} | {} | {:.2} | {:.2} | {:.2} | {:.2} | {} |\n",
                        target.name,
                        phase.phase.label(),
                        phase.operations,
                        phase.total_micros,
                        phase.ops_per_second,
                        phase.mean_micros,
                        phase.p95_micros,
                        phase.p99_micros,
                        target.storage_bytes,
                    ));
                }
            }
        }
        let diagnostic_targets = self
            .targets
            .iter()
            .filter(|target| !target.storage_diagnostics.is_empty())
            .collect::<Vec<_>>();
        if !diagnostic_targets.is_empty() {
            out.push_str("\n## Storage Diagnostics\n\n");
            for target in diagnostic_targets {
                out.push_str(&format!("### {}\n\n", target.name));
                for line in &target.storage_diagnostics {
                    if line.starts_with(' ') {
                        out.push_str(&format!("{line}\n"));
                    } else {
                        out.push_str(&format!("- {line}\n"));
                    }
                }
                out.push('\n');
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TargetReport {
    pub name: String,
    pub phases: Vec<PhaseMetrics>,
    pub storage_bytes: u64,
    pub storage_diagnostics: Vec<String>,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PhaseMetrics {
    pub phase: PhaseName,
    pub operations: u64,
    pub total_micros: u128,
    pub ops_per_second: f64,
    pub mean_micros: f64,
    pub p95_micros: f64,
    pub p99_micros: f64,
}
