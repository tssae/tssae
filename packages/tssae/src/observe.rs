use std::{collections::BTreeMap, time::Instant};

use serde::{Deserialize, Serialize};

/// Engine-side observation verbosity level.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ObserveMode {
    #[default]
    Off,
    Summary,
    Detail,
}

/// Optional engine observation configuration passed per RPC request.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObserveRequestConfig {
    #[serde(default)]
    pub mode: ObserveMode,
    #[serde(default)]
    pub max_items: Option<usize>,
    #[serde(default)]
    pub max_bytes: Option<usize>,
}

/// Structured detail entry recorded for an engine query step in detail mode.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStepDetail {
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    pub summary: String,
}

/// Per-step execution metrics returned when observation is enabled.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStepMetrics {
    pub op: &'static str,
    pub provider: String,
    pub cache_status: &'static str,
    pub duration_ms: f64,
    pub candidate_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Vec<EngineStepDetail>>,
}

/// Built-in engine fact provider extraction counters for one RPC call.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineProviderExtractionStats {
    pub extractions: usize,
    pub candidates: usize,
}

/// Engine-side IR lowering summary counters for one RPC call.
#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineLoweringMetrics {
    pub functions_lowered: usize,
    pub vertices_created: usize,
    pub duration_ms: f64,
}

/// Engine-side IR cache hit/miss and cross-snapshot file reuse/invalidation counters.
#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineIrCacheMetrics {
    pub function_hits: usize,
    pub function_misses: usize,
    pub files_reused: usize,
    pub files_invalidated: usize,
}

/// Engine-side metrics attached to an RPC response only when observation is enabled.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineRpcMetrics {
    pub duration_ms: f64,
    pub encode_ms: f64,
    pub truncated: bool,
    pub dropped_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<EngineStepMetrics>,
    pub lowering: EngineLoweringMetrics,
    pub ir_cache: EngineIrCacheMetrics,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub provider_extractions: BTreeMap<String, EngineProviderExtractionStats>,
}

/// Tracks engine-side timing, candidate counts, and bounded detail records for one RPC call.
pub struct ObserveCollector {
    mode: ObserveMode,
    max_items: Option<usize>,
    max_bytes: Option<usize>,
    started_at: Option<Instant>,
    encode_ms: f64,
    used_items: usize,
    used_bytes: usize,
    truncated: bool,
    dropped_count: usize,
    steps: Vec<EngineStepMetrics>,
    lowering: EngineLoweringMetrics,
    ir_cache: EngineIrCacheMetrics,
    provider_extractions: BTreeMap<String, EngineProviderExtractionStats>,
}

impl ObserveCollector {
    #[must_use]
    pub fn from_request(config: Option<ObserveRequestConfig>) -> Self {
        let cfg = config.unwrap_or_default();
        let started_at = if cfg.mode == ObserveMode::Off {
            None
        } else {
            Some(Instant::now())
        };
        Self {
            mode: cfg.mode,
            max_items: cfg.max_items,
            max_bytes: cfg.max_bytes,
            started_at,
            encode_ms: 0.0,
            used_items: 0,
            used_bytes: 0,
            truncated: false,
            dropped_count: 0,
            steps: Vec::new(),
            lowering: EngineLoweringMetrics::default(),
            ir_cache: EngineIrCacheMetrics::default(),
            provider_extractions: BTreeMap::new(),
        }
    }

    #[must_use]
    pub const fn is_active(&self) -> bool {
        !matches!(self.mode, ObserveMode::Off)
    }

    #[must_use]
    pub const fn is_detail(&self) -> bool {
        matches!(self.mode, ObserveMode::Detail)
    }

    #[must_use]
    pub fn start_step(&self) -> Option<Instant> {
        if self.is_active() {
            Some(Instant::now())
        } else {
            None
        }
    }

    pub fn add_encode_ms(&mut self, duration_ms: f64) {
        if self.is_active() {
            self.encode_ms += duration_ms;
        }
    }

    pub const fn record_ir_cache_hit(&mut self) {
        if self.is_active() {
            self.ir_cache.function_hits += 1;
        }
    }

    pub fn record_ir_lowering(
        &mut self,
        vertices_created: usize,
        duration_ms: f64,
        succeeded: bool,
    ) {
        if !self.is_active() {
            return;
        }
        self.ir_cache.function_misses += 1;
        if succeeded {
            self.lowering.functions_lowered += 1;
            self.lowering.vertices_created += vertices_created;
        }
        self.lowering.duration_ms += duration_ms;
    }

    pub const fn record_ir_file_reuse(&mut self, files_reused: usize, files_invalidated: usize) {
        if self.is_active() {
            self.ir_cache.files_reused += files_reused;
            self.ir_cache.files_invalidated += files_invalidated;
        }
    }

    pub fn record_step(
        &mut self,
        op: &'static str,
        step_start: Option<Instant>,
        candidate_count: usize,
        raw_details: Vec<EngineStepDetail>,
    ) {
        self.push_step_metrics(
            op,
            "engine",
            "computed",
            step_start,
            candidate_count,
            raw_details,
        );
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Observation recording takes step identity, cache status, timing, and detail items."
    )]
    pub fn record_provider_step_with_cache(
        &mut self,
        op: &'static str,
        provider: &str,
        engine_provider_id: &str,
        cache_status: &'static str,
        step_start: Option<Instant>,
        candidate_count: usize,
        raw_details: Vec<EngineStepDetail>,
    ) {
        if !self.is_active() {
            return;
        }
        if cache_status == "computed" {
            let stats = self
                .provider_extractions
                .entry(engine_provider_id.to_string())
                .or_default();
            stats.extractions += 1;
            stats.candidates += candidate_count;
        }
        self.push_step_metrics(
            op,
            provider,
            cache_status,
            step_start,
            candidate_count,
            raw_details,
        );
    }

    fn push_step_metrics(
        &mut self,
        op: &'static str,
        provider: &str,
        cache_status: &'static str,
        step_start: Option<Instant>,
        candidate_count: usize,
        raw_details: Vec<EngineStepDetail>,
    ) {
        if !self.is_active() {
            return;
        }

        let duration_ms = step_start.map_or(0.0, elapsed_ms);
        let details = if self.is_detail() {
            let mut kept = Vec::new();
            for item in raw_details {
                if self.truncated {
                    self.dropped_count += 1;
                    continue;
                }
                if let Some(limit) = self.max_items
                    && self.used_items >= limit
                {
                    self.truncated = true;
                    self.dropped_count += 1;
                    continue;
                }

                let encode_start = Instant::now();
                let item_bytes = serde_json::to_string(&item).map_or(0, |s| s.len());
                self.encode_ms += elapsed_ms(encode_start);

                if let Some(byte_limit) = self.max_bytes
                    && self.used_bytes + item_bytes > byte_limit
                {
                    self.truncated = true;
                    self.dropped_count += 1;
                    continue;
                }

                self.used_items += 1;
                self.used_bytes += item_bytes;
                kept.push(item);
            }
            Some(kept)
        } else {
            None
        };

        self.steps.push(EngineStepMetrics {
            op,
            provider: provider.to_string(),
            cache_status,
            duration_ms,
            candidate_count,
            details,
        });
    }

    #[must_use]
    pub fn finish(self) -> Option<EngineRpcMetrics> {
        let started_at = self.started_at?;
        Some(EngineRpcMetrics {
            duration_ms: elapsed_ms(started_at),
            encode_ms: self.encode_ms,
            truncated: self.truncated,
            dropped_count: self.dropped_count,
            steps: self.steps,
            lowering: self.lowering,
            ir_cache: self.ir_cache,
            provider_extractions: self.provider_extractions,
        })
    }
}

#[must_use]
pub fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}
