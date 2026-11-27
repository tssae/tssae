use std::{cmp::Ordering, sync::LazyLock};

use rustc_hash::FxHashMap;
use tssae_common::{
    BehaviorSummaryEntry, CallAspectSummary, CallbackCardinality, CallbackTiming,
    DeclarationIdentity, EffectAspectSummary, ForwardCallMode, ForwardCallReceiver,
    ForwardCallSpec, ForwardCallTarget, InstanceAspectSummary, InstanceOutcome, InvokeCallbackSpec,
    ResourceOpSpec, SummaryAspectKind, SummaryCompleteness, SummaryConfigPayload,
    SummaryConflictError, SummaryConflictParty, SummaryNamedValueRef, SummarySlotRef,
    SummarySource, SummaryTaggedValueRef, SummaryTargetMatcher, SummaryThisArg,
    SummaryThisUndefined, SummaryValueRef, ValueAspectSummary, ValueFlowSpec,
};

#[derive(Clone, Debug)]
struct IndexedEntry {
    source: SummarySource,
    pack_id: String,
    origin_file: Option<String>,
    entry: BehaviorSummaryEntry,
}

/// Matched aspect payload together with its provenance metadata.
#[derive(Clone, Debug)]
pub struct MatchedSummaryAspect<T> {
    pub source: SummarySource,
    pub pack_id: String,
    pub entry_id: Option<String>,
    pub origin_file: Option<String>,
    pub matcher: SummaryTargetMatcher,
    pub aspect: T,
}

/// Snapshot-scoped behavior summary registry supporting 4-tier priority, semver matching, and conflict detection.
#[derive(Clone, Debug)]
pub struct BehaviorSummaryIndex {
    tiers: [Vec<IndexedEntry>; 4],
    resolved_package_versions: FxHashMap<String, Option<String>>,
}

static BUILTIN_STDLIB_ENTRIES: LazyLock<Vec<BehaviorSummaryEntry>> =
    LazyLock::new(builtin_stdlib_summaries);

static DEFAULT_BUILTIN_INDEX: LazyLock<BehaviorSummaryIndex> = LazyLock::new(|| {
    BehaviorSummaryIndex::build_uncached(&SummaryConfigPayload::default(), &FxHashMap::default())
});

impl Default for BehaviorSummaryIndex {
    fn default() -> Self {
        Self::with_builtins_only()
    }
}

impl BehaviorSummaryIndex {
    #[must_use]
    pub fn with_builtins_only() -> Self {
        DEFAULT_BUILTIN_INDEX.clone()
    }

    #[must_use]
    pub fn from_config(
        config: &SummaryConfigPayload,
        resolved_package_versions: &FxHashMap<String, Option<String>>,
    ) -> Self {
        if config.packs.is_empty() && config.enable_builtin {
            return DEFAULT_BUILTIN_INDEX.clone();
        }
        Self::build_uncached(config, resolved_package_versions)
    }

    fn build_uncached(
        config: &SummaryConfigPayload,
        resolved_package_versions: &FxHashMap<String, Option<String>>,
    ) -> Self {
        let mut tiers: [Vec<IndexedEntry>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for tagged in &config.packs {
            let idx = tier_index(tagged.source);
            for entry in &tagged.pack.entries {
                tiers[idx].push(IndexedEntry {
                    source: tagged.source,
                    pack_id: tagged.pack.id.clone(),
                    origin_file: tagged.origin_file.clone(),
                    entry: entry.clone(),
                });
            }
        }
        if config.enable_builtin {
            let builtin_idx = tier_index(SummarySource::Builtin);
            for entry in &*BUILTIN_STDLIB_ENTRIES {
                tiers[builtin_idx].push(IndexedEntry {
                    source: SummarySource::Builtin,
                    pack_id: "builtin:stdlib".to_string(),
                    origin_file: None,
                    entry: entry.clone(),
                });
            }
        }
        Self {
            tiers,
            resolved_package_versions: resolved_package_versions.clone(),
        }
    }

    /// Resolves the highest-priority `calls` aspect matching `identity`.
    ///
    /// # Errors
    /// Returns [`SummaryConflictError`] if two `complete` entries in the winning tier disagree.
    pub fn resolve_calls_summary(
        &self,
        identity: &DeclarationIdentity,
        is_construct: bool,
    ) -> Result<Option<MatchedSummaryAspect<CallAspectSummary>>, SummaryConflictError> {
        self.resolve_aspect(
            identity,
            is_construct,
            SummaryAspectKind::Calls,
            |e| e.calls.as_ref(),
            |c| c.completeness,
        )
    }

    /// Resolves the highest-priority `values` aspect matching `identity`.
    ///
    /// # Errors
    /// Returns [`SummaryConflictError`] if two `complete` entries in the winning tier disagree.
    pub fn resolve_values_summary(
        &self,
        identity: &DeclarationIdentity,
        is_construct: bool,
    ) -> Result<Option<MatchedSummaryAspect<ValueAspectSummary>>, SummaryConflictError> {
        self.resolve_aspect(
            identity,
            is_construct,
            SummaryAspectKind::Values,
            |e| e.values.as_ref(),
            |v| v.completeness,
        )
    }

    /// Resolves the highest-priority `instances` aspect matching `identity`.
    ///
    /// # Errors
    /// Returns [`SummaryConflictError`] if two `complete` entries in the winning tier disagree.
    pub fn resolve_instances_summary(
        &self,
        identity: &DeclarationIdentity,
        is_construct: bool,
    ) -> Result<Option<MatchedSummaryAspect<InstanceAspectSummary>>, SummaryConflictError> {
        self.resolve_aspect(
            identity,
            is_construct,
            SummaryAspectKind::Instances,
            |e| e.instances.as_ref(),
            |i| i.completeness,
        )
    }

    /// Resolves the highest-priority `effects` aspect matching `identity`.
    ///
    /// # Errors
    /// Returns [`SummaryConflictError`] if two `complete` entries in the winning tier disagree.
    pub fn resolve_effects_summary(
        &self,
        identity: &DeclarationIdentity,
        is_construct: bool,
    ) -> Result<Option<MatchedSummaryAspect<EffectAspectSummary>>, SummaryConflictError> {
        self.resolve_aspect(
            identity,
            is_construct,
            SummaryAspectKind::Effects,
            |e| e.effects.as_ref(),
            |ef| ef.completeness,
        )
    }

    fn resolve_aspect<T: Clone + PartialEq + serde::Serialize>(
        &self,
        identity: &DeclarationIdentity,
        is_construct: bool,
        aspect_kind: SummaryAspectKind,
        extract: impl Fn(&BehaviorSummaryEntry) -> Option<&T>,
        completeness_of: impl Fn(&T) -> SummaryCompleteness,
    ) -> Result<Option<MatchedSummaryAspect<T>>, SummaryConflictError> {
        for tier in &self.tiers {
            let mut matched: Vec<(&IndexedEntry, &T)> = Vec::new();
            for item in tier {
                let Some(aspect) = extract(&item.entry) else {
                    continue;
                };
                if matcher_matches_identity(
                    &item.entry.matcher,
                    identity,
                    is_construct,
                    &self.resolved_package_versions,
                ) {
                    matched.push((item, aspect));
                }
            }
            if matched.is_empty() {
                continue;
            }
            return select_or_conflict(
                &matched,
                aspect_kind,
                &describe_identity(identity),
                &completeness_of,
            )
            .map(Some);
        }
        Ok(None)
    }
}

const fn tier_index(source: SummarySource) -> usize {
    match source {
        SummarySource::Local => 0,
        SummarySource::Package => 1,
        SummarySource::Community => 2,
        SummarySource::Builtin => 3,
    }
}

fn describe_identity(identity: &DeclarationIdentity) -> String {
    let path_str = identity
        .member_paths
        .first()
        .map_or_else(|| "<symbol>".to_string(), |p| p.join("."));
    match (&identity.package_name, &identity.module_path) {
        (Some(pkg), _) => format!("{pkg}:{path_str}"),
        (None, Some(module)) => format!("{module}:{path_str}"),
        (None, None) => path_str,
    }
}

fn select_or_conflict<T: Clone + PartialEq + serde::Serialize>(
    matched: &[(&IndexedEntry, &T)],
    aspect_kind: SummaryAspectKind,
    target_description: &str,
    completeness_of: &impl Fn(&T) -> SummaryCompleteness,
) -> Result<MatchedSummaryAspect<T>, SummaryConflictError> {
    let complete_matches: Vec<(&IndexedEntry, &T)> = matched
        .iter()
        .copied()
        .filter(|(_, aspect)| completeness_of(aspect) == SummaryCompleteness::Complete)
        .collect();

    if complete_matches.len() >= 2 {
        let (first_item, first_aspect) = complete_matches[0];
        for &(other_item, other_aspect) in &complete_matches[1..] {
            if first_aspect != other_aspect {
                return Err(SummaryConflictError {
                    source: first_item.source,
                    aspect: aspect_kind,
                    target_description: target_description.to_string(),
                    reason: format!(
                        "two complete summaries in the '{}' tier specify conflicting '{}' behaviors",
                        first_item.source.as_str(),
                        aspect_kind.as_str()
                    ),
                    parties: Box::new([
                        SummaryConflictParty {
                            source: first_item.source,
                            pack_id: first_item.pack_id.clone(),
                            entry_id: first_item.entry.id.clone(),
                            origin_file: first_item.origin_file.clone(),
                            matcher: first_item.entry.matcher.clone(),
                            aspect: aspect_kind,
                            completeness: SummaryCompleteness::Complete,
                            summary: serde_json::to_value(first_aspect)
                                .unwrap_or(serde_json::Value::Null),
                        },
                        SummaryConflictParty {
                            source: other_item.source,
                            pack_id: other_item.pack_id.clone(),
                            entry_id: other_item.entry.id.clone(),
                            origin_file: other_item.origin_file.clone(),
                            matcher: other_item.entry.matcher.clone(),
                            aspect: aspect_kind,
                            completeness: SummaryCompleteness::Complete,
                            summary: serde_json::to_value(other_aspect)
                                .unwrap_or(serde_json::Value::Null),
                        },
                    ]),
                });
            }
        }
    }

    let (chosen_item, chosen_aspect) = complete_matches.first().copied().unwrap_or(matched[0]);
    Ok(MatchedSummaryAspect {
        source: chosen_item.source,
        pack_id: chosen_item.pack_id.clone(),
        entry_id: chosen_item.entry.id.clone(),
        origin_file: chosen_item.origin_file.clone(),
        matcher: chosen_item.entry.matcher.clone(),
        aspect: chosen_aspect.clone(),
    })
}

fn normalize_module_path(path: &str) -> &str {
    path.strip_prefix("./").unwrap_or(path)
}

fn normalize_export_path(path: &str) -> &str {
    if path == "./" { "." } else { path }
}

fn matcher_matches_identity(
    matcher: &SummaryTargetMatcher,
    identity: &DeclarationIdentity,
    is_construct: bool,
    resolved_package_versions: &FxHashMap<String, Option<String>>,
) -> bool {
    if let Some(pkg) = &matcher.package {
        if identity.package_name.as_deref() != Some(pkg.as_str()) {
            return false;
        }
        if let Some(expected_export) = &matcher.export_path {
            let norm_expected = normalize_export_path(expected_export);
            match identity.export_path.as_deref() {
                Some(actual_export) => {
                    if normalize_export_path(actual_export) != norm_expected {
                        return false;
                    }
                }
                None => {
                    if norm_expected != "." {
                        return false;
                    }
                }
            }
        }
        if let Some(ver_range) = &matcher.version {
            let pkg_ver = identity.package_version.as_deref().or_else(|| {
                resolved_package_versions
                    .get(pkg)
                    .and_then(Option::as_deref)
            });
            let Some(actual_ver) = pkg_ver else {
                return false;
            };
            if !semver_matches(actual_ver, ver_range) {
                return false;
            }
        }
    } else {
        if identity.package_name.is_some() {
            return false;
        }
        if let Some(expected_mod) = &matcher.module {
            let Some(actual_mod) = identity.module_path.as_deref() else {
                return false;
            };
            if normalize_module_path(actual_mod) != normalize_module_path(expected_mod) {
                return false;
            }
        }
    }

    if matcher.member_path.get(1).map(String::as_str) == Some("prototype")
        && !identity.is_instance_member
    {
        return false;
    }
    if matcher.member_path.last().map(String::as_str) == Some("constructor") && !is_construct {
        return identity
            .member_paths
            .iter()
            .any(|p| p == &matcher.member_path);
    }
    identity
        .member_paths
        .iter()
        .any(|p| p == &matcher.member_path)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemverVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub prerelease: Vec<PrereleaseIdent>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrereleaseIdent {
    Numeric(u64),
    Alpha(String),
}

impl Ord for PrereleaseIdent {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Numeric(a), Self::Numeric(b)) => a.cmp(b),
            (Self::Numeric(_), Self::Alpha(_)) => Ordering::Less,
            (Self::Alpha(_), Self::Numeric(_)) => Ordering::Greater,
            (Self::Alpha(a), Self::Alpha(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for PrereleaseIdent {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SemverVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch)) {
            Ordering::Equal => match (self.prerelease.is_empty(), other.prerelease.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.prerelease.cmp(&other.prerelease),
            },
            non_eq => non_eq,
        }
    }
}

impl PartialOrd for SemverVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CompOp {
    Lt,
    Lte,
    Gt,
    Gte,
    Eq,
}

#[derive(Clone, Debug)]
struct Comparator {
    op: CompOp,
    ver: SemverVersion,
}

fn parse_u64_strict(part: &str) -> Option<u64> {
    if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if part.len() > 1 && part.starts_with('0') {
        return None;
    }
    part.parse::<u64>().ok()
}

fn parse_prerelease_segment(part: &str) -> Option<PrereleaseIdent> {
    if part.is_empty() || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return None;
    }
    if part.bytes().all(|b| b.is_ascii_digit()) {
        parse_u64_strict(part).map(PrereleaseIdent::Numeric)
    } else {
        Some(PrereleaseIdent::Alpha(part.to_string()))
    }
}

/// Parses a strict `MAJOR.MINOR.PATCH[-PRERELEASE][+BUILD]` semver string.
#[must_use]
pub fn parse_semver_version(raw: &str) -> Option<SemverVersion> {
    let trimmed = raw.trim().strip_prefix('v').unwrap_or_else(|| raw.trim());
    let without_build = trimmed.split('+').next()?;
    let (core, pre_opt) = match without_build.split_once('-') {
        Some((c, p)) => {
            if p.is_empty() {
                return None;
            }
            (c, Some(p))
        }
        None => (without_build, None),
    };
    let mut parts = core.split('.');
    let major = parse_u64_strict(parts.next()?)?;
    let minor = parse_u64_strict(parts.next()?)?;
    let patch = parse_u64_strict(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    let prerelease = if let Some(pre) = pre_opt {
        pre.split('.')
            .map(parse_prerelease_segment)
            .collect::<Option<Vec<_>>>()?
    } else {
        Vec::new()
    };
    Some(SemverVersion {
        major,
        minor,
        patch,
        prerelease,
    })
}

fn is_wildcard_part(s: &str) -> bool {
    matches!(s, "*" | "x" | "X")
}

#[derive(Clone, Debug)]
struct PartialVersion {
    major: Option<u64>,
    minor: Option<u64>,
    patch: Option<u64>,
    prerelease: Vec<PrereleaseIdent>,
}

fn parse_partial_version(raw: &str) -> Option<PartialVersion> {
    let trimmed = raw.trim().strip_prefix('v').unwrap_or_else(|| raw.trim());
    if trimmed.is_empty() {
        return None;
    }
    let without_build = trimmed.split('+').next()?;
    let (core, pre_opt) = match without_build.split_once('-') {
        Some((c, p)) => {
            if p.is_empty() {
                return None;
            }
            (c, Some(p))
        }
        None => (without_build, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    let major = if is_wildcard_part(parts[0]) {
        None
    } else {
        Some(parse_u64_strict(parts[0])?)
    };
    let minor = match parts.get(1) {
        None => None,
        Some(&p) if is_wildcard_part(p) => None,
        Some(&p) => {
            major?;
            Some(parse_u64_strict(p)?)
        }
    };
    let patch = match parts.get(2) {
        None => None,
        Some(&p) if is_wildcard_part(p) => None,
        Some(&p) => {
            major?;
            minor?;
            Some(parse_u64_strict(p)?)
        }
    };
    let prerelease = if let Some(pre) = pre_opt {
        if major.is_none() || minor.is_none() || patch.is_none() {
            return None;
        }
        pre.split('.')
            .map(parse_prerelease_segment)
            .collect::<Option<Vec<_>>>()?
    } else {
        Vec::new()
    };
    Some(PartialVersion {
        major,
        minor,
        patch,
        prerelease,
    })
}

fn expand_caret_comparator(rest: &str) -> Option<Vec<Comparator>> {
    let pv = parse_partial_version(rest)?;
    let Some(major) = pv.major else {
        return Some(vec![Comparator {
            op: CompOp::Gte,
            ver: zero_ver(),
        }]);
    };
    let lower = SemverVersion {
        major,
        minor: pv.minor.unwrap_or(0),
        patch: pv.patch.unwrap_or(0),
        prerelease: pv.prerelease,
    };
    let upper = if major > 0 {
        SemverVersion {
            major: major + 1,
            minor: 0,
            patch: 0,
            prerelease: Vec::new(),
        }
    } else if let Some(minor) = pv.minor {
        if minor > 0 {
            SemverVersion {
                major: 0,
                minor: minor + 1,
                patch: 0,
                prerelease: Vec::new(),
            }
        } else if let Some(patch) = pv.patch {
            SemverVersion {
                major: 0,
                minor: 0,
                patch: patch + 1,
                prerelease: Vec::new(),
            }
        } else {
            SemverVersion {
                major: 0,
                minor: 1,
                patch: 0,
                prerelease: Vec::new(),
            }
        }
    } else {
        SemverVersion {
            major: 1,
            minor: 0,
            patch: 0,
            prerelease: Vec::new(),
        }
    };
    Some(vec![
        Comparator {
            op: CompOp::Gte,
            ver: lower,
        },
        Comparator {
            op: CompOp::Lt,
            ver: upper,
        },
    ])
}

fn expand_tilde_comparator(rest: &str) -> Option<Vec<Comparator>> {
    let pv = parse_partial_version(rest)?;
    let Some(major) = pv.major else {
        return Some(vec![Comparator {
            op: CompOp::Gte,
            ver: zero_ver(),
        }]);
    };
    let lower = SemverVersion {
        major,
        minor: pv.minor.unwrap_or(0),
        patch: pv.patch.unwrap_or(0),
        prerelease: pv.prerelease,
    };
    let upper = pv.minor.map_or_else(
        || SemverVersion {
            major: major + 1,
            minor: 0,
            patch: 0,
            prerelease: Vec::new(),
        },
        |minor| SemverVersion {
            major,
            minor: minor + 1,
            patch: 0,
            prerelease: Vec::new(),
        },
    );
    Some(vec![
        Comparator {
            op: CompOp::Gte,
            ver: lower,
        },
        Comparator {
            op: CompOp::Lt,
            ver: upper,
        },
    ])
}

fn split_comparator_op(t: &str) -> (Option<CompOp>, &str) {
    for (prefix, op) in [
        (">=", CompOp::Gte),
        ("<=", CompOp::Lte),
        (">", CompOp::Gt),
        ("<", CompOp::Lt),
        ("=", CompOp::Eq),
    ] {
        if let Some(rest) = t.strip_prefix(prefix) {
            return (Some(op), rest);
        }
    }
    (None, t)
}

fn expand_single_comparator(token: &str) -> Option<Vec<Comparator>> {
    let t = token.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(rest) = t.strip_prefix('^') {
        return expand_caret_comparator(rest);
    }
    if let Some(rest) = t.strip_prefix('~') {
        return expand_tilde_comparator(rest);
    }

    let (explicit_op, rest) = split_comparator_op(t);

    let pv = parse_partial_version(rest)?;
    match explicit_op {
        Some(op) => {
            let ver = SemverVersion {
                major: pv.major.unwrap_or(0),
                minor: pv.minor.unwrap_or(0),
                patch: pv.patch.unwrap_or(0),
                prerelease: pv.prerelease,
            };
            if pv.major.is_none() && op == CompOp::Eq {
                return Some(vec![Comparator {
                    op: CompOp::Gte,
                    ver: zero_ver(),
                }]);
            }
            Some(vec![Comparator { op, ver }])
        }
        None => Some(expand_x_range(&pv)),
    }
}

fn expand_x_range(pv: &PartialVersion) -> Vec<Comparator> {
    let Some(major) = pv.major else {
        return vec![Comparator {
            op: CompOp::Gte,
            ver: zero_ver(),
        }];
    };
    let Some(minor) = pv.minor else {
        return vec![
            Comparator {
                op: CompOp::Gte,
                ver: SemverVersion {
                    major,
                    minor: 0,
                    patch: 0,
                    prerelease: Vec::new(),
                },
            },
            Comparator {
                op: CompOp::Lt,
                ver: SemverVersion {
                    major: major + 1,
                    minor: 0,
                    patch: 0,
                    prerelease: Vec::new(),
                },
            },
        ];
    };
    let Some(patch) = pv.patch else {
        return vec![
            Comparator {
                op: CompOp::Gte,
                ver: SemverVersion {
                    major,
                    minor,
                    patch: 0,
                    prerelease: Vec::new(),
                },
            },
            Comparator {
                op: CompOp::Lt,
                ver: SemverVersion {
                    major,
                    minor: minor + 1,
                    patch: 0,
                    prerelease: Vec::new(),
                },
            },
        ];
    };
    vec![Comparator {
        op: CompOp::Eq,
        ver: SemverVersion {
            major,
            minor,
            patch,
            prerelease: pv.prerelease.clone(),
        },
    }]
}

const fn zero_ver() -> SemverVersion {
    SemverVersion {
        major: 0,
        minor: 0,
        patch: 0,
        prerelease: Vec::new(),
    }
}

fn parse_comparator_set(clause: &str) -> Option<Vec<Comparator>> {
    let trimmed = clause.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some((left_raw, right_raw)) = trimmed.split_once(" - ") {
        let left = parse_partial_version(left_raw)?;
        let right = parse_partial_version(right_raw)?;
        let lower = Comparator {
            op: CompOp::Gte,
            ver: SemverVersion {
                major: left.major.unwrap_or(0),
                minor: left.minor.unwrap_or(0),
                patch: left.patch.unwrap_or(0),
                prerelease: left.prerelease,
            },
        };
        let upper = match (right.major, right.minor, right.patch) {
            (None, _, _) => None,
            (Some(maj), None, _) => Some(Comparator {
                op: CompOp::Lt,
                ver: SemverVersion {
                    major: maj + 1,
                    minor: 0,
                    patch: 0,
                    prerelease: Vec::new(),
                },
            }),
            (Some(maj), Some(min), None) => Some(Comparator {
                op: CompOp::Lt,
                ver: SemverVersion {
                    major: maj,
                    minor: min + 1,
                    patch: 0,
                    prerelease: Vec::new(),
                },
            }),
            (Some(maj), Some(min), Some(pat)) => Some(Comparator {
                op: CompOp::Lte,
                ver: SemverVersion {
                    major: maj,
                    minor: min,
                    patch: pat,
                    prerelease: right.prerelease,
                },
            }),
        };
        let mut out = vec![lower];
        if let Some(u) = upper {
            out.push(u);
        }
        return Some(out);
    }

    let mut out = Vec::new();
    for token in trimmed.split_whitespace() {
        out.extend(expand_single_comparator(token)?);
    }
    if out.is_empty() { None } else { Some(out) }
}

fn comparator_matches(comp: &Comparator, ver: &SemverVersion) -> bool {
    match comp.op {
        CompOp::Eq => ver == &comp.ver,
        CompOp::Lt => ver < &comp.ver,
        CompOp::Lte => ver <= &comp.ver,
        CompOp::Gt => ver > &comp.ver,
        CompOp::Gte => ver >= &comp.ver,
    }
}

/// Evaluates whether a concrete `version` satisfies an npm-style semver `range`.
#[must_use]
pub fn semver_matches(version: &str, range: &str) -> bool {
    let Some(ver) = parse_semver_version(version) else {
        return false;
    };
    let clauses: Option<Vec<Vec<Comparator>>> =
        range.split("||").map(parse_comparator_set).collect();
    let Some(clauses) = clauses else {
        return false;
    };

    clauses.iter().any(|set| {
        if !set.iter().all(|c| comparator_matches(c, &ver)) {
            return false;
        }
        if ver.prerelease.is_empty() {
            return true;
        }
        set.iter().any(|c| {
            !c.ver.prerelease.is_empty()
                && c.ver.major == ver.major
                && c.ver.minor == ver.minor
                && c.ver.patch == ver.patch
        })
    })
}

fn mk_entry(
    id: &str,
    member_path: &[&str],
    calls: Option<CallAspectSummary>,
    values: Option<ValueAspectSummary>,
    instances: Option<InstanceAspectSummary>,
    effects: Option<EffectAspectSummary>,
) -> BehaviorSummaryEntry {
    BehaviorSummaryEntry {
        id: Some(id.to_string()),
        matcher: SummaryTargetMatcher {
            package: None,
            version: None,
            export_path: None,
            module: None,
            member_path: member_path.iter().map(|s| (*s).to_string()).collect(),
        },
        description: None,
        calls,
        values,
        instances,
        effects,
    }
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "Optional aspect builder used directly in mk_entry arguments."
)]
const fn complete_calls(
    invokes_callbacks: Vec<InvokeCallbackSpec>,
    forwards_call: Option<ForwardCallSpec>,
) -> Option<CallAspectSummary> {
    Some(CallAspectSummary {
        completeness: SummaryCompleteness::Complete,
        invokes_callbacks,
        forwards_call,
    })
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "Optional aspect builder used directly in mk_entry arguments."
)]
const fn complete_values(flows: Vec<ValueFlowSpec>) -> Option<ValueAspectSummary> {
    Some(ValueAspectSummary {
        completeness: SummaryCompleteness::Complete,
        flows,
    })
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "Optional aspect builder used directly in mk_entry arguments."
)]
const fn complete_instances(outcome: InstanceOutcome) -> Option<InstanceAspectSummary> {
    Some(InstanceAspectSummary {
        completeness: SummaryCompleteness::Complete,
        outcome,
    })
}

const fn val_flow(from: SummaryValueRef, to: SummaryValueRef) -> ValueFlowSpec {
    ValueFlowSpec {
        from,
        to,
        preserve_object_path: None,
    }
}

const fn v_recv_elem() -> SummaryValueRef {
    SummaryValueRef::Named(SummaryNamedValueRef::ReceiverElement)
}

const fn v_ret() -> SummaryValueRef {
    SummaryValueRef::Named(SummaryNamedValueRef::Return)
}

const fn v_ret_elem() -> SummaryValueRef {
    SummaryValueRef::Named(SummaryNamedValueRef::ReturnElement)
}

const fn v_arg(index: usize) -> SummaryValueRef {
    SummaryValueRef::Tagged(SummaryTaggedValueRef::Arg { index })
}

const fn v_arg_elem(index: usize) -> SummaryValueRef {
    SummaryValueRef::Tagged(SummaryTaggedValueRef::ArgElement { index })
}

const fn v_cb_param(callback_arg_index: usize, param_index: usize) -> SummaryValueRef {
    SummaryValueRef::Tagged(SummaryTaggedValueRef::CallbackParam {
        callback_arg_index,
        param_index,
    })
}

const fn v_cb_ret(callback_arg_index: usize) -> SummaryValueRef {
    SummaryValueRef::Tagged(SummaryTaggedValueRef::CallbackReturn { callback_arg_index })
}

const fn sync_cb(callback_arg_index: usize, this_arg_index: Option<usize>) -> InvokeCallbackSpec {
    InvokeCallbackSpec {
        callback_arg_index,
        timing: CallbackTiming::Synchronous,
        cardinality: Some(CallbackCardinality::ZeroOrMore),
        this_arg: match this_arg_index {
            Some(idx) => Some(SummaryThisArg::FromArg {
                from_arg_index: idx,
            }),
            None => None,
        },
        event_name: None,
    }
}

const fn deferred_cb(callback_arg_index: usize) -> InvokeCallbackSpec {
    InvokeCallbackSpec {
        callback_arg_index,
        timing: CallbackTiming::Deferred,
        cardinality: Some(CallbackCardinality::AtMostOnce),
        this_arg: Some(SummaryThisArg::Undefined(SummaryThisUndefined::Undefined)),
        event_name: None,
    }
}

/// Built-in ECMAScript standard library behavior summaries shipped inside the engine binary.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "Declarative catalog of built-in Array, ReadonlyArray, Promise, Function, Object, Map, and Set summaries."
)]
pub fn builtin_stdlib_summaries() -> Vec<BehaviorSummaryEntry> {
    let mut entries = Vec::new();

    for container in ["Array", "ReadonlyArray"] {
        entries.push(mk_entry(
            &format!("builtin:{container}.prototype.forEach"),
            &[container, "prototype", "forEach"],
            complete_calls(vec![sync_cb(0, Some(1))], None),
            complete_values(vec![val_flow(v_recv_elem(), v_cb_param(0, 0))]),
            complete_instances(InstanceOutcome::None),
            Some(EffectAspectSummary {
                completeness: SummaryCompleteness::Complete,
                resource_ops: Vec::new(),
                mutates_receiver: Some(false),
            }),
        ));

        for mapper in ["map", "flatMap"] {
            entries.push(mk_entry(
                &format!("builtin:{container}.prototype.{mapper}"),
                &[container, "prototype", mapper],
                complete_calls(vec![sync_cb(0, Some(1))], None),
                complete_values(vec![
                    val_flow(v_recv_elem(), v_cb_param(0, 0)),
                    val_flow(v_cb_ret(0), v_ret_elem()),
                ]),
                complete_instances(InstanceOutcome::AllocatesFreshInstance),
                Some(EffectAspectSummary {
                    completeness: SummaryCompleteness::Complete,
                    resource_ops: Vec::new(),
                    mutates_receiver: Some(false),
                }),
            ));
        }

        entries.push(mk_entry(
            &format!("builtin:{container}.prototype.filter"),
            &[container, "prototype", "filter"],
            complete_calls(vec![sync_cb(0, Some(1))], None),
            complete_values(vec![
                val_flow(v_recv_elem(), v_cb_param(0, 0)),
                val_flow(v_recv_elem(), v_ret_elem()),
            ]),
            complete_instances(InstanceOutcome::AllocatesFreshInstance),
            None,
        ));

        for finder in ["find", "findLast"] {
            entries.push(mk_entry(
                &format!("builtin:{container}.prototype.{finder}"),
                &[container, "prototype", finder],
                complete_calls(vec![sync_cb(0, Some(1))], None),
                complete_values(vec![
                    val_flow(v_recv_elem(), v_cb_param(0, 0)),
                    val_flow(v_recv_elem(), v_ret()),
                ]),
                complete_instances(InstanceOutcome::None),
                None,
            ));
        }

        for predicate_method in ["findIndex", "findLastIndex", "some", "every"] {
            entries.push(mk_entry(
                &format!("builtin:{container}.prototype.{predicate_method}"),
                &[container, "prototype", predicate_method],
                complete_calls(vec![sync_cb(0, Some(1))], None),
                complete_values(vec![val_flow(v_recv_elem(), v_cb_param(0, 0))]),
                complete_instances(InstanceOutcome::None),
                None,
            ));
        }

        for reducer in ["reduce", "reduceRight"] {
            entries.push(mk_entry(
                &format!("builtin:{container}.prototype.{reducer}"),
                &[container, "prototype", reducer],
                complete_calls(vec![sync_cb(0, None)], None),
                complete_values(vec![
                    val_flow(v_arg(1), v_cb_param(0, 0)),
                    val_flow(v_cb_ret(0), v_cb_param(0, 0)),
                    val_flow(v_recv_elem(), v_cb_param(0, 1)),
                    val_flow(v_cb_ret(0), v_ret()),
                    val_flow(v_arg(1), v_ret()),
                ]),
                None,
                None,
            ));
        }

        entries.push(mk_entry(
            &format!("builtin:{container}.prototype.sort"),
            &[container, "prototype", "sort"],
            complete_calls(vec![sync_cb(0, None)], None),
            complete_values(vec![
                val_flow(v_recv_elem(), v_cb_param(0, 0)),
                val_flow(v_recv_elem(), v_cb_param(0, 1)),
                val_flow(v_recv_elem(), v_ret_elem()),
            ]),
            complete_instances(InstanceOutcome::ForwardsInstance {
                from: SummarySlotRef::Receiver(ForwardCallReceiver::Receiver),
            }),
            Some(EffectAspectSummary {
                completeness: SummaryCompleteness::Complete,
                resource_ops: Vec::new(),
                mutates_receiver: Some(true),
            }),
        ));

        entries.push(mk_entry(
            &format!("builtin:{container}.prototype.toSorted"),
            &[container, "prototype", "toSorted"],
            complete_calls(vec![sync_cb(0, None)], None),
            complete_values(vec![
                val_flow(v_recv_elem(), v_cb_param(0, 0)),
                val_flow(v_recv_elem(), v_cb_param(0, 1)),
                val_flow(v_recv_elem(), v_ret_elem()),
            ]),
            complete_instances(InstanceOutcome::AllocatesFreshInstance),
            Some(EffectAspectSummary {
                completeness: SummaryCompleteness::Complete,
                resource_ops: Vec::new(),
                mutates_receiver: Some(false),
            }),
        ));
    }

    entries.push(mk_entry(
        "builtin:Array.from",
        &["Array", "from"],
        complete_calls(vec![sync_cb(1, Some(2))], None),
        complete_values(vec![
            val_flow(v_arg_elem(0), v_cb_param(1, 0)),
            val_flow(v_cb_ret(1), v_ret_elem()),
            val_flow(v_arg_elem(0), v_ret_elem()),
        ]),
        complete_instances(InstanceOutcome::AllocatesFreshInstance),
        None,
    ));

    entries.push(mk_entry(
        "builtin:Array.isArray",
        &["Array", "isArray"],
        complete_calls(Vec::new(), None),
        complete_values(Vec::new()),
        complete_instances(InstanceOutcome::None),
        Some(EffectAspectSummary {
            completeness: SummaryCompleteness::Complete,
            resource_ops: Vec::new(),
            mutates_receiver: Some(false),
        }),
    ));

    entries.push(mk_entry(
        "builtin:Promise.constructor",
        &["Promise", "constructor"],
        complete_calls(
            vec![InvokeCallbackSpec {
                callback_arg_index: 0,
                timing: CallbackTiming::Synchronous,
                cardinality: Some(CallbackCardinality::Once),
                this_arg: Some(SummaryThisArg::Undefined(SummaryThisUndefined::Undefined)),
                event_name: None,
            }],
            None,
        ),
        complete_values(vec![val_flow(
            SummaryValueRef::Tagged(SummaryTaggedValueRef::CallbackParamArg {
                callback_arg_index: 0,
                callback_param_index: 0,
                arg_index: 0,
            }),
            v_ret_elem(),
        )]),
        complete_instances(InstanceOutcome::AllocatesFreshInstance),
        None,
    ));

    for promise_iface in ["Promise", "PromiseLike"] {
        entries.push(mk_entry(
            &format!("builtin:{promise_iface}.prototype.then"),
            &[promise_iface, "prototype", "then"],
            complete_calls(vec![deferred_cb(0), deferred_cb(1)], None),
            complete_values(vec![
                val_flow(v_recv_elem(), v_cb_param(0, 0)),
                val_flow(v_cb_ret(0), v_ret_elem()),
                val_flow(v_cb_ret(1), v_ret_elem()),
            ]),
            complete_instances(InstanceOutcome::AllocatesFreshInstance),
            None,
        ));
    }

    entries.push(mk_entry(
        "builtin:Promise.prototype.catch",
        &["Promise", "prototype", "catch"],
        complete_calls(vec![deferred_cb(0)], None),
        complete_values(vec![
            val_flow(v_recv_elem(), v_ret_elem()),
            val_flow(v_cb_ret(0), v_ret_elem()),
        ]),
        complete_instances(InstanceOutcome::AllocatesFreshInstance),
        None,
    ));

    entries.push(mk_entry(
        "builtin:Promise.prototype.finally",
        &["Promise", "prototype", "finally"],
        complete_calls(vec![deferred_cb(0)], None),
        complete_values(vec![val_flow(v_recv_elem(), v_ret_elem())]),
        complete_instances(InstanceOutcome::AllocatesFreshInstance),
        None,
    ));

    entries.push(mk_entry(
        "builtin:Promise.resolve",
        &["Promise", "resolve"],
        complete_calls(Vec::new(), None),
        complete_values(vec![val_flow(v_arg(0), v_ret_elem())]),
        complete_instances(InstanceOutcome::AllocatesFreshInstance),
        None,
    ));

    entries.push(mk_entry(
        "builtin:Promise.reject",
        &["Promise", "reject"],
        complete_calls(Vec::new(), None),
        complete_values(Vec::new()),
        complete_instances(InstanceOutcome::AllocatesFreshInstance),
        None,
    ));

    for comb in ["all", "allSettled", "any", "race"] {
        entries.push(mk_entry(
            &format!("builtin:Promise.{comb}"),
            &["Promise", comb],
            complete_calls(Vec::new(), None),
            complete_values(vec![val_flow(v_arg_elem(0), v_ret_elem())]),
            complete_instances(InstanceOutcome::AllocatesFreshInstance),
            None,
        ));
    }

    entries.push(mk_entry(
        "builtin:Function.prototype.call",
        &["Function", "prototype", "call"],
        complete_calls(
            Vec::new(),
            Some(ForwardCallSpec {
                target: ForwardCallTarget::Receiver(ForwardCallReceiver::Receiver),
                mode: ForwardCallMode::Call,
                this_arg: Some(SummaryThisArg::FromArg { from_arg_index: 0 }),
                args_from_index: Some(1),
                args_array_index: None,
            }),
        ),
        None,
        None,
        None,
    ));

    entries.push(mk_entry(
        "builtin:Function.prototype.apply",
        &["Function", "prototype", "apply"],
        complete_calls(
            Vec::new(),
            Some(ForwardCallSpec {
                target: ForwardCallTarget::Receiver(ForwardCallReceiver::Receiver),
                mode: ForwardCallMode::Apply,
                this_arg: Some(SummaryThisArg::FromArg { from_arg_index: 0 }),
                args_from_index: None,
                args_array_index: Some(1),
            }),
        ),
        None,
        None,
        None,
    ));

    entries.push(mk_entry(
        "builtin:Function.prototype.bind",
        &["Function", "prototype", "bind"],
        complete_calls(
            Vec::new(),
            Some(ForwardCallSpec {
                target: ForwardCallTarget::Receiver(ForwardCallReceiver::Receiver),
                mode: ForwardCallMode::Bind,
                this_arg: Some(SummaryThisArg::FromArg { from_arg_index: 0 }),
                args_from_index: Some(1),
                args_array_index: None,
            }),
        ),
        None,
        complete_instances(InstanceOutcome::AllocatesFreshInstance),
        None,
    ));

    entries.push(mk_entry(
        "builtin:Object.assign",
        &["Object", "assign"],
        complete_calls(Vec::new(), None),
        complete_values(vec![
            val_flow(v_arg(0), v_ret()),
            val_flow(v_arg(1), v_ret()),
        ]),
        complete_instances(InstanceOutcome::ForwardsInstance {
            from: SummarySlotRef::Arg { arg_index: 0 },
        }),
        None,
    ));

    for obj_method in ["entries", "values", "keys", "fromEntries"] {
        entries.push(mk_entry(
            &format!("builtin:Object.{obj_method}"),
            &["Object", obj_method],
            complete_calls(Vec::new(), None),
            complete_values(vec![val_flow(v_arg_elem(0), v_ret_elem())]),
            complete_instances(InstanceOutcome::AllocatesFreshInstance),
            None,
        ));
    }

    for col in ["Map", "Set"] {
        entries.push(mk_entry(
            &format!("builtin:{col}.prototype.forEach"),
            &[col, "prototype", "forEach"],
            complete_calls(vec![sync_cb(0, Some(1))], None),
            complete_values(vec![val_flow(v_recv_elem(), v_cb_param(0, 0))]),
            complete_instances(InstanceOutcome::None),
            None,
        ));
    }

    entries.push(mk_entry(
        "builtin:Map.prototype.set",
        &["Map", "prototype", "set"],
        complete_calls(Vec::new(), None),
        complete_values(vec![val_flow(v_arg(1), v_ret_elem())]),
        complete_instances(InstanceOutcome::ForwardsInstance {
            from: SummarySlotRef::Receiver(ForwardCallReceiver::Receiver),
        }),
        Some(EffectAspectSummary {
            completeness: SummaryCompleteness::Complete,
            resource_ops: Vec::new(),
            mutates_receiver: Some(true),
        }),
    ));

    entries.push(mk_entry(
        "builtin:Set.prototype.add",
        &["Set", "prototype", "add"],
        complete_calls(Vec::new(), None),
        complete_values(vec![val_flow(v_arg(0), v_ret_elem())]),
        complete_instances(InstanceOutcome::ForwardsInstance {
            from: SummarySlotRef::Receiver(ForwardCallReceiver::Receiver),
        }),
        Some(EffectAspectSummary {
            completeness: SummaryCompleteness::Complete,
            resource_ops: Vec::new(),
            mutates_receiver: Some(true),
        }),
    ));

    for (iface, sym_key) in [
        ("Disposable", "[Symbol.dispose]"),
        ("AsyncDisposable", "[Symbol.asyncDispose]"),
    ] {
        entries.push(mk_entry(
            &format!("builtin:{iface}.prototype.{sym_key}"),
            &[iface, "prototype", sym_key],
            None,
            None,
            None,
            Some(EffectAspectSummary {
                completeness: SummaryCompleteness::Complete,
                resource_ops: vec![ResourceOpSpec::DisposesTarget {
                    target: SummarySlotRef::Receiver(ForwardCallReceiver::Receiver),
                }],
                mutates_receiver: Some(true),
            }),
        ));
    }

    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use tssae_common::TaggedSummaryPack;

    #[test]
    fn semver_range_matching_handles_caret_tilde_and_or() {
        assert!(semver_matches("1.2.3", "^1.0.0"));
        assert!(!semver_matches("2.0.0", "^1.0.0"));
        assert!(semver_matches("1.2.9", "~1.2.3"));
        assert!(!semver_matches("1.3.0", "~1.2.3"));
        assert!(semver_matches("2.1.0", "^1.0.0 || ^2.0.0"));
        assert!(semver_matches("1.5.0", "1.2.0 - 2.0.0"));
        assert!(!semver_matches("1.0.0-alpha.1", "^1.0.0"));
        assert!(semver_matches("1.0.0-alpha.2", ">=1.0.0-alpha.1 <1.0.0"));
    }

    #[test]
    fn higher_tier_overrides_lower_tier_per_aspect() {
        let pkg_pack = TaggedSummaryPack {
            source: SummarySource::Package,
            origin_file: Some("node_modules/foo/summaries.json".to_string()),
            pack: tssae_common::BehaviorSummaryPack {
                id: "pkg:foo".to_string(),
                version: None,
                description: None,
                entries: vec![BehaviorSummaryEntry {
                    id: Some("e1".to_string()),
                    matcher: SummaryTargetMatcher {
                        package: Some("foo".to_string()),
                        version: Some("^1.0.0".to_string()),
                        export_path: None,
                        module: None,
                        member_path: vec!["run".to_string()],
                    },
                    description: None,
                    calls: complete_calls(vec![sync_cb(0, None)], None),
                    values: complete_values(vec![val_flow(v_arg(0), v_ret())]),
                    instances: None,
                    effects: None,
                }],
            },
        };
        let local_pack = TaggedSummaryPack {
            source: SummarySource::Local,
            origin_file: Some("tssae.summaries.json".to_string()),
            pack: tssae_common::BehaviorSummaryPack {
                id: "local".to_string(),
                version: None,
                description: None,
                entries: vec![BehaviorSummaryEntry {
                    id: Some("e2".to_string()),
                    matcher: SummaryTargetMatcher {
                        package: Some("foo".to_string()),
                        version: None,
                        export_path: None,
                        module: None,
                        member_path: vec!["run".to_string()],
                    },
                    description: None,
                    calls: complete_calls(Vec::new(), None),
                    values: None,
                    instances: None,
                    effects: None,
                }],
            },
        };
        let mut versions = FxHashMap::default();
        versions.insert("foo".to_string(), Some("1.4.0".to_string()));
        let index = BehaviorSummaryIndex::from_config(
            &SummaryConfigPayload {
                packs: vec![pkg_pack, local_pack],
                enable_builtin: true,
            },
            &versions,
        );
        let identity = DeclarationIdentity {
            package_name: Some("foo".to_string()),
            package_version: Some("1.4.0".to_string()),
            export_path: Some(".".to_string()),
            module_path: None,
            is_default_library: false,
            member_paths: vec![vec!["run".to_string()]],
            is_instance_member: false,
        };
        let calls = index
            .resolve_calls_summary(&identity, false)
            .unwrap()
            .unwrap();
        assert_eq!(calls.source, SummarySource::Local);
        assert_eq!(calls.aspect.invokes_callbacks.len(), 0);

        let values = index
            .resolve_values_summary(&identity, false)
            .unwrap()
            .unwrap();
        assert_eq!(values.source, SummarySource::Package);
        assert_eq!(values.aspect.flows.len(), 1);
    }

    #[test]
    fn same_tier_conflicting_complete_summaries_return_conflict_error() {
        let pack_a = TaggedSummaryPack {
            source: SummarySource::Community,
            origin_file: Some("node_modules/@tssae-summaries/foo/index.json".to_string()),
            pack: tssae_common::BehaviorSummaryPack {
                id: "comm-a".to_string(),
                version: None,
                description: None,
                entries: vec![BehaviorSummaryEntry {
                    id: Some("a1".to_string()),
                    matcher: SummaryTargetMatcher {
                        package: Some("foo".to_string()),
                        version: None,
                        export_path: None,
                        module: None,
                        member_path: vec!["run".to_string()],
                    },
                    description: None,
                    calls: complete_calls(vec![sync_cb(0, None)], None),
                    values: None,
                    instances: None,
                    effects: None,
                }],
            },
        };
        let pack_b = TaggedSummaryPack {
            source: SummarySource::Community,
            origin_file: Some("node_modules/@tssae-summaries/foo/alt.json".to_string()),
            pack: tssae_common::BehaviorSummaryPack {
                id: "comm-b".to_string(),
                version: None,
                description: None,
                entries: vec![BehaviorSummaryEntry {
                    id: Some("b1".to_string()),
                    matcher: SummaryTargetMatcher {
                        package: Some("foo".to_string()),
                        version: None,
                        export_path: None,
                        module: None,
                        member_path: vec!["run".to_string()],
                    },
                    description: None,
                    calls: complete_calls(Vec::new(), None),
                    values: None,
                    instances: None,
                    effects: None,
                }],
            },
        };
        let index = BehaviorSummaryIndex::from_config(
            &SummaryConfigPayload {
                packs: vec![pack_a, pack_b],
                enable_builtin: false,
            },
            &FxHashMap::default(),
        );
        let identity = DeclarationIdentity {
            package_name: Some("foo".to_string()),
            package_version: None,
            export_path: Some(".".to_string()),
            module_path: None,
            is_default_library: false,
            member_paths: vec![vec!["run".to_string()]],
            is_instance_member: false,
        };
        let err = index.resolve_calls_summary(&identity, false).unwrap_err();
        assert_eq!(err.source, SummarySource::Community);
        assert_eq!(err.parties[0].pack_id, "comm-a");
        assert_eq!(err.parties[1].pack_id, "comm-b");
    }

    #[test]
    fn default_builtin_index_cache_ignores_package_versions_and_matches_builtins() {
        for entry in &*BUILTIN_STDLIB_ENTRIES {
            assert!(
                entry.matcher.package.is_none(),
                "builtin entry {:?} must not specify package",
                entry.id
            );
            assert!(
                entry.matcher.version.is_none(),
                "builtin entry {:?} must not specify version",
                entry.id
            );
        }

        let mut versions = FxHashMap::default();
        versions.insert("lodash".to_string(), Some("4.17.21".to_string()));
        let cached = BehaviorSummaryIndex::from_config(&SummaryConfigPayload::default(), &versions);
        let fresh = BehaviorSummaryIndex::with_builtins_only();

        let array_for_each = DeclarationIdentity {
            package_name: None,
            package_version: None,
            export_path: None,
            module_path: None,
            is_default_library: true,
            member_paths: vec![vec![
                "Array".to_string(),
                "prototype".to_string(),
                "forEach".to_string(),
            ]],
            is_instance_member: true,
        };
        let cached_match = cached
            .resolve_calls_summary(&array_for_each, false)
            .unwrap()
            .unwrap();
        let fresh_match = fresh
            .resolve_calls_summary(&array_for_each, false)
            .unwrap()
            .unwrap();
        assert_eq!(cached_match.entry_id, fresh_match.entry_id);
        assert_eq!(cached_match.aspect, fresh_match.aspect);
    }
}
