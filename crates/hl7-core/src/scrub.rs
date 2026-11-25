//! Deterministic, referentially-consistent HL7 v2 PHI de-identification ("Scrub PHI").
//!
//! Uses keyed `HMAC-SHA256` with a session or user-supplied seed so the same patient MRN,
//! name, encounter number, DOB, address, phone number, and email address map to the exact
//! same synthetic identity across an entire batch of `ADT`, `ORM`, and `ORU` messages,
//! followed by a whole-body, boundary-aware sweep across all non-structural fields of every
//! segment (plus standalone email detection and cross-message patient token pooling).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::{Date, Duration, Month};

use crate::address::FieldAddress;
use crate::delimiters::Delimiters;
use crate::record::Record;

/// Scrubbing strategy for a user-configured [`CustomPhiRule`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CustomPhiMode {
    /// Deterministic keyed-hash synthetic replacement (preserves referential consistency).
    #[default]
    Synthetic,
    /// Fixed redaction token (`[REDACTED]`).
    Redact,
    /// Format-preserving mask (replaces alphanumeric characters with `*`).
    Mask,
}

impl CustomPhiMode {
    /// All modes in dropdown display order.
    pub const ALL: [Self; 3] = [Self::Synthetic, Self::Redact, Self::Mask];

    /// Human-readable label for UI dropdowns and tooltips.
    pub fn label(self) -> &'static str {
        match self {
            Self::Synthetic => "Synthetic",
            Self::Redact => "Redact",
            Self::Mask => "Mask",
        }
    }

    /// Compact 3-character badge tag displayed inside custom PHI pills.
    pub fn short_tag(self) -> &'static str {
        match self {
            Self::Synthetic => "SYN",
            Self::Redact => "RED",
            Self::Mask => "MSK",
        }
    }
}

/// One user-configured custom HL7 PHI field rule (e.g. `ZPI-4`, `PID-18.1`, `OBX-5`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomPhiRule {
    /// Canonical HL7 path (e.g. `"ZPI-4"`, `"PID-18.1"`, `"OBX-5"`).
    pub path: String,
    /// Scrubbing strategy for matched values.
    #[serde(default)]
    pub mode: CustomPhiMode,
}

impl CustomPhiRule {
    /// Parse a CLI `--phi-field` specification (`"ZPI-4"`, `"ZPI-4:redact"`, `"OBX-5:mask"`, `"PID-18.1:synthetic"`).
    pub fn parse_cli_spec(spec: &str) -> Result<Self, String> {
        let trimmed = spec.trim();
        let (path_part, mode) = match trimmed.rsplit_once(':') {
            Some((p, m)) => {
                let parsed_mode = match m.trim().to_ascii_lowercase().as_str() {
                    "synthetic" | "syn" => CustomPhiMode::Synthetic,
                    "redact" | "red" => CustomPhiMode::Redact,
                    "mask" | "msk" => CustomPhiMode::Mask,
                    other => {
                        return Err(format!(
                            "Invalid scrub mode '{other}' in '{trimmed}': expected synthetic, redact, or mask."
                        ));
                    }
                };
                (p.trim(), parsed_mode)
            }
            None => (trimmed, CustomPhiMode::Synthetic),
        };
        let addr: FieldAddress = path_part
            .parse()
            .map_err(|e| format!("Invalid HL7 path '{path_part}': {e}"))?;
        if addr.is_header_delimiter_field() {
            return Err(format!(
                "Cannot scrub header delimiter field '{path_part}'."
            ));
        }
        Ok(Self {
            path: addr.to_string(),
            mode,
        })
    }
}

/// Optional configuration for [`PhiScrubber`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhiScrubConfig {
    /// Additional user-configured custom HL7 paths to scrub alongside standard Safe Harbor fields.
    #[serde(default)]
    pub extra_paths: Vec<CustomPhiRule>,
}

/// Count how many non-empty field/component/repetition values across `records` match `rules`,
/// respecting each record's dynamic `MSH-1`/`MSH-2` delimiters.
pub fn count_custom_phi_matches(records: &[Record], rules: &[CustomPhiRule]) -> usize {
    let parsed_rules: Vec<FieldAddress> = rules
        .iter()
        .filter_map(|r| r.path.trim().parse::<FieldAddress>().ok())
        .filter(|a| !a.is_header_delimiter_field())
        .collect();
    if parsed_rules.is_empty() || records.is_empty() {
        return 0;
    }

    let mut total = 0usize;
    for rec in records {
        let delims = rec.delimiters();
        let charset = rec.charset();
        let seg_count = rec.segments().len();
        for seg_idx in 0..seg_count {
            let Some(seg_bytes) = rec.segment_bytes(seg_idx) else {
                continue;
            };
            if seg_bytes.len() < 4 || seg_bytes[3] != delims.field {
                continue;
            }
            let tag = [seg_bytes[0], seg_bytes[1], seg_bytes[2]];
            if tag == *b"MSH" || tag == *b"FHS" || tag == *b"BHS" {
                continue;
            }
            let fields: Vec<String> = seg_bytes[4..]
                .split(|&b| b == delims.field)
                .map(|b| charset.decode_display(b).into_owned())
                .collect();
            for addr in &parsed_rules {
                if addr.segment != tag {
                    continue;
                }
                let f_idx = usize::from(addr.field.get().saturating_sub(1));
                let Some(field_val) = fields.get(f_idx) else {
                    continue;
                };
                if field_val.trim().is_empty() {
                    continue;
                }
                total += count_matches_in_field(field_val, *addr, delims);
            }
        }
    }
    total
}

fn count_matches_in_field(field_val: &str, addr: FieldAddress, delims: Delimiters) -> usize {
    let rep_sep = char::from(delims.repetition);
    let comp_sep = char::from(delims.component);
    let sub_sep = char::from(delims.subcomponent);
    let mut hits = 0usize;
    for (r_idx, rep) in field_val.split(rep_sep).enumerate() {
        if let Some(target_r) = addr.repetition {
            if r_idx + 1 != usize::from(target_r.get()) {
                continue;
            }
        }
        let target_val = if let Some(target_c) = addr.component {
            let Some(comp) = rep.split(comp_sep).nth(usize::from(target_c.get() - 1)) else {
                continue;
            };
            if let Some(target_s) = addr.subcomponent {
                let Some(sub) = comp.split(sub_sep).nth(usize::from(target_s.get() - 1)) else {
                    continue;
                };
                sub
            } else {
                comp
            }
        } else {
            rep
        };
        if !target_val.trim().is_empty() {
            hits += 1;
        }
    }
    hits
}

/// Bold heading for the mandatory best-effort PHI de-identification disclaimer.
pub const PHI_SCRUB_DISCLAIMER_HEADING: &str = "WARNING — Best-Effort De-Identification Only:";

/// Body text for the mandatory best-effort PHI de-identification disclaimer.
pub const PHI_SCRUB_DISCLAIMER_BODY: &str = "Automated PHI scrubbing CANNOT be 100% guaranteed to detect or remove all Protected Health Information (including custom Z-segments, unstructured clinical notes, or embedded attachments). You are solely responsible for manually inspecting and verifying all scrubbed output before sharing or using it outside a secure environment.";

/// Combined single-string best-effort PHI de-identification disclaimer for CLI `--help` and status output.
pub const PHI_SCRUB_DISCLAIMER: &str = "WARNING — Best-Effort De-Identification Only: Automated PHI scrubbing CANNOT be 100% guaranteed to detect or remove all Protected Health Information (including custom Z-segments, unstructured clinical notes, or embedded attachments). You are solely responsible for manually inspecting and verifying all scrubbed output before sharing or using it outside a secure environment.";

/// Deterministic, keyed-hash HL7 v2 PHI de-identifier.
#[derive(Debug, Clone)]
pub struct PhiScrubber {
    seed: String,
    extra_paths: Vec<CustomPhiRule>,
}

impl PhiScrubber {
    /// Create a scrubber with an explicit deterministic seed string.
    pub fn new(seed: impl Into<String>) -> Self {
        let s = seed.into();
        let trimmed = s.trim();
        if trimmed.is_empty() {
            Self::with_random_seed()
        } else {
            Self {
                seed: trimmed.to_string(),
                extra_paths: Vec::new(),
            }
        }
    }

    /// Create a scrubber with a fresh random per-session UUID seed.
    pub fn with_random_seed() -> Self {
        Self {
            seed: uuid::Uuid::new_v4().to_string(),
            extra_paths: Vec::new(),
        }
    }

    /// Attach additional custom HL7 PHI path rules (`Z*` segments, `OBX-5`, `PID-18.1`, etc.).
    pub fn with_extra_paths(mut self, extra_paths: Vec<CustomPhiRule>) -> Self {
        self.extra_paths = extra_paths;
        self
    }

    /// Attach a [`PhiScrubConfig`] to this scrubber.
    pub fn with_config(mut self, config: PhiScrubConfig) -> Self {
        self.extra_paths = config.extra_paths;
        self
    }

    /// Return the configured custom PHI path rules.
    pub fn extra_paths(&self) -> &[CustomPhiRule] {
        &self.extra_paths
    }

    /// Return the active seed string (useful for reproducing a mapping).
    pub fn seed(&self) -> &str {
        &self.seed
    }

    /// Scrub a batch of records with full cross-message referential consistency and
    /// patient-keyed PHI token pooling (`PID-3`).
    pub fn scrub_records(&self, records: &[Record]) -> Vec<Record> {
        if records.is_empty() {
            return Vec::new();
        }

        // Batch Pass 1: Scrub structured PHI fields and pool detected tokens per patient key.
        let mut pass1_results: Vec<Pass1Record> = Vec::with_capacity(records.len());
        let mut patient_pools: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();

        for rec in records {
            let p1 = self.scrub_record_pass1(rec);
            if let Some(ref key) = p1.patient_key {
                let pool = patient_pools.entry(key.clone()).or_default();
                for (orig, synth) in &p1.sweep_tokens {
                    pool.entry(orig.clone()).or_insert_with(|| synth.clone());
                }
            }
            pass1_results.push(p1);
        }

        // Batch Pass 2: Whole-body sweep using local + patient-pooled tokens and standalone email scrubbing.
        records
            .iter()
            .zip(pass1_results)
            .map(|(rec, p1)| {
                let pooled = p1.patient_key.as_deref().and_then(|k| patient_pools.get(k));
                self.finish_record_pass2(rec, p1, pooled)
            })
            .collect()
    }

    /// Scrub a single [`Record`], preserving its delimiters, terminator metadata, and structure.
    pub fn scrub_record(&self, record: &Record) -> Record {
        let p1 = self.scrub_record_pass1(record);
        self.finish_record_pass2(record, p1, None)
    }

    fn scrub_record_pass1(&self, record: &Record) -> Pass1Record {
        let delims = record.delimiters();
        let charset = record.charset();
        let seg_count = record.segments().len();

        // Derive a stable normalized patient key for DOB shifting and cross-message token pooling.
        let patient_key = extract_patient_key(record, &charset).map(|k| k.to_ascii_uppercase());

        // Pass 1: Scrub targeted PHI fields and collect (original -> synthetic) tokens for sweep.
        let mut sweep_tokens: BTreeMap<String, String> = BTreeMap::new();
        let mut scrubbed_segments: Vec<Vec<u8>> = Vec::with_capacity(seg_count);

        for seg_idx in 0..seg_count {
            let Some(seg_bytes) = record.segment_bytes(seg_idx) else {
                continue;
            };
            if seg_bytes.len() < 4 {
                scrubbed_segments.push(seg_bytes.to_vec());
                continue;
            }
            let tag = [seg_bytes[0], seg_bytes[1], seg_bytes[2]];
            if tag == *b"MSH" || tag == *b"FHS" || tag == *b"BHS" || seg_bytes[3] != delims.field {
                scrubbed_segments.push(seg_bytes.to_vec());
                continue;
            }

            let mut fields: Vec<String> = seg_bytes[4..]
                .split(|&b| b == delims.field)
                .map(|b| charset.decode_display(b).into_owned())
                .collect();

            match &tag {
                b"PID" => {
                    self.scrub_cx_id_field(&mut fields, 2, delims, &mut sweep_tokens);
                    self.scrub_cx_id_field(&mut fields, 3, delims, &mut sweep_tokens);
                    self.scrub_cx_id_field(&mut fields, 4, delims, &mut sweep_tokens);
                    self.scrub_xpn_field(&mut fields, 5, delims, &mut sweep_tokens);
                    self.scrub_dob_field(&mut fields, 7, patient_key.as_deref(), &mut sweep_tokens);
                    self.scrub_xpn_field(&mut fields, 9, delims, &mut sweep_tokens);
                    self.scrub_xad_field(&mut fields, 11, delims, &mut sweep_tokens);
                    self.scrub_scalar_id_field(&mut fields, 12, None);
                    self.scrub_xtn_field(&mut fields, 13, delims, &mut sweep_tokens);
                    self.scrub_xtn_field(&mut fields, 14, delims, &mut sweep_tokens);
                    self.scrub_cx_id_field(&mut fields, 18, delims, &mut sweep_tokens);
                    self.scrub_scalar_id_field(&mut fields, 19, Some(&mut sweep_tokens));
                }
                b"PV1" => {
                    self.scrub_cx_id_field(&mut fields, 19, delims, &mut sweep_tokens);
                    self.scrub_cx_id_field(&mut fields, 50, delims, &mut sweep_tokens);
                }
                b"ORC" => {
                    self.scrub_cx_id_field(&mut fields, 2, delims, &mut sweep_tokens);
                    self.scrub_cx_id_field(&mut fields, 3, delims, &mut sweep_tokens);
                    self.scrub_cx_id_field(&mut fields, 4, delims, &mut sweep_tokens);
                }
                b"OBR" => {
                    self.scrub_cx_id_field(&mut fields, 2, delims, &mut sweep_tokens);
                    self.scrub_cx_id_field(&mut fields, 3, delims, &mut sweep_tokens);
                }
                b"TXA" => {
                    self.scrub_cx_id_field(&mut fields, 12, delims, &mut sweep_tokens);
                    self.scrub_cx_id_field(&mut fields, 13, delims, &mut sweep_tokens);
                }
                b"SCH" => {
                    self.scrub_cx_id_field(&mut fields, 1, delims, &mut sweep_tokens);
                    self.scrub_cx_id_field(&mut fields, 2, delims, &mut sweep_tokens);
                    self.scrub_cx_id_field(&mut fields, 5, delims, &mut sweep_tokens);
                    self.scrub_xtn_field(&mut fields, 13, delims, &mut sweep_tokens);
                    self.scrub_xtn_field(&mut fields, 17, delims, &mut sweep_tokens);
                }
                b"RXE" => {
                    // Preserve RXE-2 (Give Code / NDC / RxNorm); scrub RXE-15 Prescription Number.
                    self.scrub_scalar_id_field(&mut fields, 15, Some(&mut sweep_tokens));
                }
                b"RXD" => {
                    // Preserve RXD-2 (Dispense/Give Code / NDC / RxNorm); scrub RXD-7 Prescription Number.
                    self.scrub_scalar_id_field(&mut fields, 7, Some(&mut sweep_tokens));
                }
                b"FT1" => {
                    // Scrub FT1-2 Transaction ID & FT1-3 Batch ID; preserve FT1-6, FT1-7, FT1-10..12, FT1-19, FT1-25.
                    self.scrub_scalar_id_field(&mut fields, 2, Some(&mut sweep_tokens));
                    self.scrub_scalar_id_field(&mut fields, 3, Some(&mut sweep_tokens));
                }
                b"NK1" => {
                    self.scrub_xpn_field(&mut fields, 2, delims, &mut sweep_tokens);
                    self.scrub_xad_field(&mut fields, 4, delims, &mut sweep_tokens);
                    self.scrub_xtn_field(&mut fields, 5, delims, &mut sweep_tokens);
                    self.scrub_xtn_field(&mut fields, 6, delims, &mut sweep_tokens);
                }
                b"GT1" => {
                    self.scrub_cx_id_field(&mut fields, 2, delims, &mut sweep_tokens);
                    self.scrub_xpn_field(&mut fields, 3, delims, &mut sweep_tokens);
                    self.scrub_xpn_field(&mut fields, 4, delims, &mut sweep_tokens);
                    self.scrub_xad_field(&mut fields, 5, delims, &mut sweep_tokens);
                    self.scrub_xtn_field(&mut fields, 6, delims, &mut sweep_tokens);
                    self.scrub_xtn_field(&mut fields, 7, delims, &mut sweep_tokens);
                    self.scrub_dob_field(&mut fields, 8, patient_key.as_deref(), &mut sweep_tokens);
                    self.scrub_scalar_id_field(&mut fields, 12, Some(&mut sweep_tokens));
                    self.scrub_org_name_field(&mut fields, 16, delims, &mut sweep_tokens);
                    self.scrub_xad_field(&mut fields, 17, delims, &mut sweep_tokens);
                    self.scrub_xtn_field(&mut fields, 18, delims, &mut sweep_tokens);
                }
                b"IN1" => {
                    // Preserve IN1-3 / IN1-4 payer ID/name; scrub member/group/employer/insured PHI.
                    self.scrub_cx_id_field(&mut fields, 2, delims, &mut sweep_tokens);
                    self.scrub_org_name_field(&mut fields, 9, delims, &mut sweep_tokens);
                    self.scrub_cx_id_field(&mut fields, 10, delims, &mut sweep_tokens);
                    self.scrub_org_name_field(&mut fields, 11, delims, &mut sweep_tokens);
                    self.scrub_xpn_field(&mut fields, 16, delims, &mut sweep_tokens);
                    self.scrub_dob_field(
                        &mut fields,
                        18,
                        patient_key.as_deref(),
                        &mut sweep_tokens,
                    );
                    self.scrub_xad_field(&mut fields, 19, delims, &mut sweep_tokens);
                    self.scrub_scalar_id_field(&mut fields, 36, Some(&mut sweep_tokens));
                    self.scrub_cx_id_field(&mut fields, 49, delims, &mut sweep_tokens);
                }
                b"IN2" => {
                    self.scrub_cx_id_field(&mut fields, 1, delims, &mut sweep_tokens);
                    self.scrub_scalar_id_field(&mut fields, 2, Some(&mut sweep_tokens));
                    self.scrub_scalar_id_field(&mut fields, 6, Some(&mut sweep_tokens));
                    self.scrub_xpn_field(&mut fields, 8, delims, &mut sweep_tokens);
                    self.scrub_scalar_id_field(&mut fields, 9, Some(&mut sweep_tokens));
                    self.scrub_cx_id_field(&mut fields, 61, delims, &mut sweep_tokens);
                    self.scrub_xtn_field(&mut fields, 63, delims, &mut sweep_tokens);
                    self.scrub_xtn_field(&mut fields, 64, delims, &mut sweep_tokens);
                }
                b"IN3" => {
                    self.scrub_cx_id_field(&mut fields, 2, delims, &mut sweep_tokens);
                }
                b"ACC" => {
                    self.scrub_org_name_field(&mut fields, 3, delims, &mut sweep_tokens);
                    self.scrub_xad_field(&mut fields, 11, delims, &mut sweep_tokens);
                }
                b"BLG" => {
                    self.scrub_cx_id_field(&mut fields, 3, delims, &mut sweep_tokens);
                }
                _ => {}
            }

            // Evaluate any user-configured custom PHI path rules (`extra_paths`) matching this segment.
            for rule in &self.extra_paths {
                let Ok(addr) = rule.path.trim().parse::<FieldAddress>() else {
                    continue;
                };
                if addr.segment != tag || addr.is_header_delimiter_field() {
                    continue;
                }
                self.apply_custom_phi_rule(&mut fields, addr, rule.mode, delims, &mut sweep_tokens);
            }

            scrubbed_segments.push(join_segment(&tag, delims.field, &fields));
        }

        Pass1Record {
            patient_key,
            sweep_tokens,
            scrubbed_segments,
        }
    }

    fn apply_custom_phi_rule(
        &self,
        fields: &mut [String],
        addr: FieldAddress,
        mode: CustomPhiMode,
        delims: Delimiters,
        sweep_tokens: &mut BTreeMap<String, String>,
    ) {
        let f_idx = usize::from(addr.field.get().saturating_sub(1));
        let Some(field) = fields.get_mut(f_idx) else {
            return;
        };
        if field.trim().is_empty() {
            return;
        }
        let rep_sep = char::from(delims.repetition);
        let comp_sep = char::from(delims.component);
        let sub_sep = char::from(delims.subcomponent);

        let mut reps: Vec<String> = field.split(rep_sep).map(str::to_string).collect();
        let mut changed = false;

        for (r_idx, rep) in reps.iter_mut().enumerate() {
            if let Some(target_r) = addr.repetition {
                if r_idx + 1 != usize::from(target_r.get()) {
                    continue;
                }
            }
            if let Some(target_c) = addr.component {
                let c_idx = usize::from(target_c.get() - 1);
                let mut comps: Vec<String> = rep.split(comp_sep).map(str::to_string).collect();
                if let Some(comp) = comps.get_mut(c_idx) {
                    if let Some(target_s) = addr.subcomponent {
                        let s_idx = usize::from(target_s.get() - 1);
                        let mut subs: Vec<String> =
                            comp.split(sub_sep).map(str::to_string).collect();
                        if let Some(sub) = subs.get_mut(s_idx) {
                            if !sub.trim().is_empty() {
                                let orig = sub.clone();
                                let repl = self.scrub_custom_value(&orig, mode);
                                if orig.len() >= 4 {
                                    sweep_tokens.entry(orig).or_insert_with(|| repl.clone());
                                }
                                *sub = repl;
                                changed = true;
                            }
                        }
                        *comp = subs.join(&sub_sep.to_string());
                    } else if !comp.trim().is_empty() {
                        let orig = comp.clone();
                        let repl = self.scrub_custom_value(&orig, mode);
                        if orig.len() >= 4 {
                            sweep_tokens.entry(orig).or_insert_with(|| repl.clone());
                        }
                        *comp = repl;
                        changed = true;
                    }
                }
                *rep = comps.join(&comp_sep.to_string());
            } else if !rep.trim().is_empty() {
                let orig = rep.clone();
                let repl = self.scrub_custom_value(&orig, mode);
                if orig.len() >= 4 {
                    sweep_tokens.entry(orig).or_insert_with(|| repl.clone());
                }
                *rep = repl;
                changed = true;
            }
        }

        if changed {
            *field = reps.join(&rep_sep.to_string());
        }
    }

    fn scrub_custom_value(&self, original: &str, mode: CustomPhiMode) -> String {
        match mode {
            CustomPhiMode::Synthetic => {
                let digest = self.hash_bytes("CUSTOM_PHI", original);
                format!(
                    "SYN_{:02X}{:02X}{:02X}{:02X}",
                    digest[0], digest[1], digest[2], digest[3]
                )
            }
            CustomPhiMode::Redact => "[REDACTED]".to_string(),
            CustomPhiMode::Mask => {
                let has_alnum = original.chars().any(char::is_alphanumeric);
                if !has_alnum {
                    "***".to_string()
                } else {
                    original
                        .chars()
                        .map(|c| if c.is_alphanumeric() { '*' } else { c })
                        .collect()
                }
            }
        }
    }

    fn finish_record_pass2(
        &self,
        record: &Record,
        mut p1: Pass1Record,
        pooled_tokens: Option<&BTreeMap<String, String>>,
    ) -> Record {
        let delims = record.delimiters();
        let charset = record.charset();

        let mut merged_tokens = p1.sweep_tokens;
        if let Some(pool) = pooled_tokens {
            for (orig, synth) in pool {
                merged_tokens
                    .entry(orig.clone())
                    .or_insert_with(|| synth.clone());
            }
        }

        let mut pairs: Vec<(String, String)> = merged_tokens.into_iter().collect();
        // Longest original tokens first to avoid partial substring collisions.
        pairs.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));

        // Pass 2: Whole-body sweep across every segment & non-structural field/component,
        // plus standalone email detection/scrubbing across free text.
        for seg in &mut p1.scrubbed_segments {
            if seg.len() < 4 || seg[3] != delims.field {
                continue;
            }
            let tag = [seg[0], seg[1], seg[2]];
            let is_header = tag == *b"MSH" || tag == *b"FHS" || tag == *b"BHS";

            let mut fields: Vec<String> = seg[4..]
                .split(|&b| b == delims.field)
                .map(|b| charset.decode_display(b).into_owned())
                .collect();
            let obx_value_type_is_numeric_or_coded = tag == *b"OBX"
                && fields.get(1).is_some_and(|vt| {
                    matches!(
                        vt.trim().to_ascii_uppercase().as_str(),
                        "NM" | "SN" | "CE" | "CWE" | "ID" | "IS"
                    )
                });
            let mut changed = false;

            for (f_idx, val) in fields.iter_mut().enumerate() {
                if val.is_empty() {
                    continue;
                }
                // Never modify MSH-2 / FHS-2 / BHS-2 encoding characters (f_idx == 0),
                // or MSH-9 (message type, f_idx == 7), MSH-11 (processing ID, f_idx == 9),
                // MSH-12 (version ID, f_idx == 10).
                if is_header && matches!(f_idx, 0 | 7 | 9 | 10) {
                    continue;
                }
                // Clinical & flowsheet guardrails: preserve OBX-3 (flowsheet/LOINC ID) and
                // numeric/coded OBX-5 values (OBX-2 in NM, SN, CE, CWE, ID, IS).
                if tag == *b"OBX"
                    && (f_idx == 2 || (f_idx == 4 && obx_value_type_is_numeric_or_coded))
                {
                    continue;
                }
                // Pharmacy & financial/payer guardrails: preserve medication/NDC/RxNorm codes,
                // CPT/charge/diagnosis codes, quantities/amounts, and payer ID/name.
                if (tag == *b"RXO" && f_idx == 0)
                    || ((tag == *b"RXE" || tag == *b"RXD") && f_idx == 1)
                    || (tag == *b"FT1" && matches!(f_idx, 5 | 6 | 9 | 10 | 11 | 18 | 24))
                    || (tag == *b"IN1" && matches!(f_idx, 2 | 3))
                {
                    continue;
                }
                let replaced = self.sweep_hl7_field_value(val, delims, &pairs);
                if replaced != *val {
                    *val = replaced;
                    changed = true;
                }
            }

            if changed {
                *seg = join_segment(&tag, delims.field, &fields);
            }
        }

        let mut new_bytes = Vec::with_capacity(record.bytes().len());
        for (i, seg) in p1.scrubbed_segments.iter().enumerate() {
            if i > 0 {
                new_bytes.push(b'\r');
            }
            new_bytes.extend_from_slice(seg);
        }

        Record::from_normalized_bytes(
            new_bytes,
            record.kind(),
            delims,
            record.source_terminator(),
            record.flags(),
        )
    }

    fn sweep_hl7_field_value(
        &self,
        field_val: &str,
        delims: Delimiters,
        pairs: &[(String, String)],
    ) -> String {
        let rep_ch = char::from(delims.repetition);
        let comp_ch = char::from(delims.component);
        let sub_ch = char::from(delims.subcomponent);

        let mut field_changed = false;
        let new_reps: Vec<String> = field_val
            .split(rep_ch)
            .map(|rep| {
                let new_comps: Vec<String> = rep
                    .split(comp_ch)
                    .map(|comp| {
                        let new_subs: Vec<String> = comp
                            .split(sub_ch)
                            .map(|sub| {
                                if sub.is_empty() {
                                    return String::new();
                                }
                                let mut cur = if pairs.is_empty() {
                                    sub.to_string()
                                } else {
                                    sweep_free_text(sub, pairs)
                                };
                                if cur.contains('@') {
                                    cur = self.scrub_standalone_emails(&cur);
                                }
                                if cur != sub {
                                    field_changed = true;
                                }
                                cur
                            })
                            .collect();
                        new_subs.join(&sub_ch.to_string())
                    })
                    .collect();
                new_comps.join(&comp_ch.to_string())
            })
            .collect();

        if field_changed {
            new_reps.join(&rep_ch.to_string())
        } else {
            field_val.to_string()
        }
    }

    fn map_email(&self, orig: &str) -> String {
        let d = self.hash_bytes("email", orig);
        format!(
            "patient.{:02x}{:02x}{:02x}@example.invalid",
            d[0], d[1], d[2]
        )
    }

    /// Detect and deterministically scrub standalone `user@domain.tld` email addresses
    /// inside arbitrary text without altering already-synthetic `@example.invalid` addresses.
    fn scrub_standalone_emails(&self, text: &str) -> String {
        let bytes = text.as_bytes();
        let mut out = String::with_capacity(text.len());
        let mut last_copied = 0usize;
        let mut i = 0usize;

        while i < bytes.len() {
            if bytes[i] == b'@' {
                // Expand local-part left (not crossing last_copied)
                let mut start = i;
                while start > last_copied && is_email_local_byte(bytes[start - 1]) {
                    start -= 1;
                }
                while start < i && matches!(bytes[start], b'.' | b'-') {
                    start += 1;
                }

                // Expand domain right
                let mut end = i + 1;
                while end < bytes.len() && is_email_domain_byte(bytes[end]) {
                    end += 1;
                }
                while end > i + 1 && matches!(bytes[end - 1], b'.' | b'-') {
                    end -= 1;
                }

                let local = &text[start..i];
                let domain = &text[i + 1..end];
                if !local.is_empty() && is_valid_email_domain(domain) {
                    let full_email = &text[start..end];
                    if !domain.to_ascii_lowercase().ends_with(".invalid") {
                        out.push_str(&text[last_copied..start]);
                        out.push_str(&self.map_email(full_email));
                        last_copied = end;
                        i = end;
                        continue;
                    }
                }
            }
            i += 1;
        }

        if last_copied == 0 {
            text.to_string()
        } else {
            out.push_str(&text[last_copied..]);
            out
        }
    }

    fn hash_bytes(&self, domain: &str, value: &str) -> [u8; 32] {
        let norm = value.trim().to_ascii_uppercase();
        let mut msg = Vec::with_capacity(domain.len() + 1 + norm.len());
        msg.extend_from_slice(domain.as_bytes());
        msg.push(0x1F);
        msg.extend_from_slice(norm.as_bytes());
        hmac_sha256(self.seed.as_bytes(), &msg)
    }

    fn map_family_name(&self, orig: &str) -> String {
        let digest = self.hash_bytes("family_name", orig);
        let idx = usize::from(u16::from_be_bytes([digest[0], digest[1]])) % FAMILY_NAMES.len();
        FAMILY_NAMES[idx].to_string()
    }

    fn map_given_name(&self, orig: &str) -> String {
        let digest = self.hash_bytes("given_name", orig);
        let idx = usize::from(u16::from_be_bytes([digest[0], digest[1]])) % GIVEN_NAMES.len();
        GIVEN_NAMES[idx].to_string()
    }

    fn map_middle_name(&self, orig: &str) -> String {
        let trimmed = orig.trim();
        let digest = self.hash_bytes("middle_name", trimmed);
        if trimmed.len() == 1 && trimmed.as_bytes()[0].is_ascii_alphabetic() {
            let ch = b'A' + (digest[0] % 26);
            (ch as char).to_string()
        } else {
            let idx = usize::from(u16::from_be_bytes([digest[0], digest[1]])) % GIVEN_NAMES.len();
            GIVEN_NAMES[idx].to_string()
        }
    }

    /// Format-preserving deterministic identifier replacement (matching digit/alpha pattern & length).
    fn map_identifier(&self, domain: &str, orig: &str) -> String {
        let digest = self.hash_bytes(domain, orig);
        let mut out = String::with_capacity(orig.len());
        for (i, ch) in orig.chars().enumerate() {
            let b = digest[i % digest.len()].wrapping_add((i as u8).wrapping_mul(31));
            if ch.is_ascii_digit() {
                out.push((b'0' + (b % 10)) as char);
            } else if ch.is_ascii_uppercase() {
                out.push((b'A' + (b % 26)) as char);
            } else if ch.is_ascii_lowercase() {
                out.push((b'a' + (b % 26)) as char);
            } else {
                out.push(ch);
            }
        }
        out
    }

    fn map_phone(&self, orig: &str) -> String {
        let digest = self.hash_bytes("phone", orig);
        let digit_count = orig.chars().filter(char::is_ascii_digit).count();
        if digit_count == 0 {
            return orig.to_string();
        }
        // Build synthetic 555-exchange digits so phone numbers are clearly synthetic.
        let mut synth_digits = Vec::with_capacity(digit_count);
        for i in 0..digit_count {
            let d = if (digit_count >= 10 && (3..=5).contains(&i)) || (digit_count == 7 && i < 3) {
                '5'
            } else {
                let b = digest[i % digest.len()].wrapping_add((i as u8).wrapping_mul(17));
                (b'0' + (b % 10)) as char
            };
            synth_digits.push(d);
        }
        let mut out = String::with_capacity(orig.len());
        let mut d_idx = 0usize;
        for ch in orig.chars() {
            if ch.is_ascii_digit() {
                if let Some(&d) = synth_digits.get(d_idx) {
                    out.push(d);
                }
                d_idx += 1;
            } else {
                out.push(ch);
            }
        }
        out
    }

    fn shift_dob(&self, orig: &str, patient_key: Option<&str>) -> String {
        let trimmed = orig.trim();
        if trimmed.len() < 8 || !trimmed.as_bytes()[..8].iter().all(u8::is_ascii_digit) {
            return orig.to_string();
        }
        let Ok(year) = trimmed[0..4].parse::<i32>() else {
            return orig.to_string();
        };
        let Ok(month_u8) = trimmed[4..6].parse::<u8>() else {
            return orig.to_string();
        };
        let Ok(day) = trimmed[6..8].parse::<u8>() else {
            return orig.to_string();
        };
        let Ok(month) = Month::try_from(month_u8) else {
            return orig.to_string();
        };
        let Ok(date) = Date::from_calendar_date(year, month, day) else {
            return orig.to_string();
        };

        let key = patient_key.unwrap_or(trimmed);
        let digest = self.hash_bytes("dob_offset", key);
        // Deterministic offset in -180..=+180 excluding 0.
        let raw = i64::from(u16::from_be_bytes([digest[0], digest[1]]) % 360);
        let offset_days = if raw < 180 { raw - 180 } else { raw - 179 };
        let shifted = date.saturating_add(Duration::days(offset_days));
        let suffix = &trimmed[8..];
        format!(
            "{:04}{:02}{:02}{suffix}",
            shifted.year(),
            shifted.month() as u8,
            shifted.day()
        )
    }

    fn map_org_name(&self, orig: &str) -> String {
        let digest = self.hash_bytes("org_name", orig);
        let idx = usize::from(u16::from_be_bytes([digest[0], digest[1]])) % ORG_NAMES.len();
        ORG_NAMES[idx].to_string()
    }

    fn scrub_org_name_field(
        &self,
        fields: &mut [String],
        field_num: usize,
        delims: Delimiters,
        sweep: &mut BTreeMap<String, String>,
    ) {
        let Some(val) = fields.get_mut(field_num - 1) else {
            return;
        };
        if val.is_empty() {
            return;
        }
        let rep_ch = char::from(delims.repetition);
        let comp_ch = char::from(delims.component);

        let new_reps: Vec<String> = val
            .split(rep_ch)
            .map(|rep| {
                let mut comps: Vec<String> = rep.split(comp_ch).map(str::to_string).collect();
                if let Some(c0) = comps.get_mut(0) {
                    if !c0.is_empty() {
                        let orig = c0.clone();
                        let synth = self.map_org_name(&orig);
                        record_name_sweep_token(sweep, &orig, &synth);
                        *c0 = synth;
                    }
                }
                comps.join(&comp_ch.to_string())
            })
            .collect();
        *val = new_reps.join(&rep_ch.to_string());
    }

    fn scrub_xpn_field(
        &self,
        fields: &mut [String],
        field_num: usize,
        delims: Delimiters,
        sweep: &mut BTreeMap<String, String>,
    ) {
        let Some(val) = fields.get_mut(field_num - 1) else {
            return;
        };
        if val.is_empty() {
            return;
        }
        let rep_ch = char::from(delims.repetition);
        let comp_ch = char::from(delims.component);
        let sub_ch = char::from(delims.subcomponent);

        let new_reps: Vec<String> = val
            .split(rep_ch)
            .map(|rep| {
                let mut comps: Vec<String> = rep.split(comp_ch).map(str::to_string).collect();
                if let Some(c0) = comps.get_mut(0) {
                    if !c0.is_empty() {
                        // Preserve FN subcomponents if present; scrub primary surname (.1)
                        let mut subs: Vec<String> = c0.split(sub_ch).map(str::to_string).collect();
                        if let Some(s0) = subs.get_mut(0) {
                            if !s0.is_empty() {
                                let orig = s0.clone();
                                let synth = self.map_family_name(&orig);
                                record_name_sweep_token(sweep, &orig, &synth);
                                *s0 = synth;
                            }
                        }
                        *c0 = subs.join(&sub_ch.to_string());
                    }
                }
                if let Some(c1) = comps.get_mut(1) {
                    if !c1.is_empty() {
                        let orig = c1.clone();
                        let synth = self.map_given_name(&orig);
                        record_name_sweep_token(sweep, &orig, &synth);
                        *c1 = synth;
                    }
                }
                if let Some(c2) = comps.get_mut(2) {
                    if !c2.is_empty() {
                        let orig = c2.clone();
                        let synth = self.map_middle_name(&orig);
                        record_name_sweep_token(sweep, &orig, &synth);
                        *c2 = synth;
                    }
                }
                comps.join(&comp_ch.to_string())
            })
            .collect();
        *val = new_reps.join(&rep_ch.to_string());
    }

    fn scrub_cx_id_field(
        &self,
        fields: &mut [String],
        field_num: usize,
        delims: Delimiters,
        sweep: &mut BTreeMap<String, String>,
    ) {
        let Some(val) = fields.get_mut(field_num - 1) else {
            return;
        };
        if val.is_empty() {
            return;
        }
        let rep_ch = char::from(delims.repetition);
        let comp_ch = char::from(delims.component);

        let new_reps: Vec<String> = val
            .split(rep_ch)
            .map(|rep| {
                let mut comps: Vec<String> = rep.split(comp_ch).map(str::to_string).collect();
                if let Some(c0) = comps.get_mut(0) {
                    if !c0.is_empty() {
                        let orig = c0.clone();
                        let synth = self.map_identifier("id", &orig);
                        record_sweep_token(sweep, &orig, &synth);
                        *c0 = synth;
                    }
                }
                comps.join(&comp_ch.to_string())
            })
            .collect();
        *val = new_reps.join(&rep_ch.to_string());
    }

    fn scrub_scalar_id_field(
        &self,
        fields: &mut [String],
        field_num: usize,
        mut sweep: Option<&mut BTreeMap<String, String>>,
    ) {
        let Some(val) = fields.get_mut(field_num - 1) else {
            return;
        };
        if val.is_empty() {
            return;
        }
        let orig = val.clone();
        let synth = self.map_identifier("id", &orig);
        if let Some(ref mut map) = sweep {
            record_sweep_token(map, &orig, &synth);
        }
        *val = synth;
    }

    fn scrub_dob_field(
        &self,
        fields: &mut [String],
        field_num: usize,
        patient_key: Option<&str>,
        sweep: &mut BTreeMap<String, String>,
    ) {
        let Some(val) = fields.get_mut(field_num - 1) else {
            return;
        };
        if val.is_empty() {
            return;
        }
        let orig = val.clone();
        let synth = self.shift_dob(&orig, patient_key);
        if orig.len() >= 8 && synth.len() >= 8 && orig != synth {
            let o8 = &orig[..8];
            let s8 = &synth[..8];
            record_sweep_token(sweep, o8, s8);
            if o8.bytes().all(|b| b.is_ascii_digit()) && s8.bytes().all(|b| b.is_ascii_digit()) {
                // Also capture ISO (`YYYY-MM-DD`) and US (`MM/DD/YYYY`) formatted DOB variants
                let o_iso = format!("{}-{}-{}", &o8[0..4], &o8[4..6], &o8[6..8]);
                let s_iso = format!("{}-{}-{}", &s8[0..4], &s8[4..6], &s8[6..8]);
                record_sweep_token(sweep, &o_iso, &s_iso);
                let o_us = format!("{}/{}/{}", &o8[4..6], &o8[6..8], &o8[0..4]);
                let s_us = format!("{}/{}/{}", &s8[4..6], &s8[6..8], &s8[0..4]);
                record_sweep_token(sweep, &o_us, &s_us);
            }
        }
        *val = synth;
    }

    fn scrub_xad_field(
        &self,
        fields: &mut [String],
        field_num: usize,
        delims: Delimiters,
        sweep: &mut BTreeMap<String, String>,
    ) {
        let Some(val) = fields.get_mut(field_num - 1) else {
            return;
        };
        if val.is_empty() {
            return;
        }
        let rep_ch = char::from(delims.repetition);
        let comp_ch = char::from(delims.component);

        let new_reps: Vec<String> = val
            .split(rep_ch)
            .map(|rep| {
                let mut comps: Vec<String> = rep.split(comp_ch).map(str::to_string).collect();
                if let Some(c0) = comps.get_mut(0) {
                    if !c0.is_empty() {
                        let orig_street = c0.clone();
                        let d = self.hash_bytes("street", &orig_street);
                        let num = 100 + (u16::from_be_bytes([d[0], d[1]]) % 9800);
                        let st_idx = usize::from(d[2]) % STREET_NAMES.len();
                        let synth_street = format!("{num} {}", STREET_NAMES[st_idx]);
                        record_name_sweep_token(sweep, &orig_street, &synth_street);
                        *c0 = synth_street;
                    }
                }
                if let Some(c1) = comps.get_mut(1) {
                    if !c1.is_empty() {
                        let d = self.hash_bytes("apt", c1);
                        let num = 10 + (u16::from_be_bytes([d[0], d[1]]) % 980);
                        *c1 = format!("APT {num}");
                    }
                }
                if let Some(c2) = comps.get_mut(2) {
                    if !c2.is_empty() {
                        let d = self.hash_bytes("city", c2);
                        let idx = usize::from(d[0]) % CITY_NAMES.len();
                        *c2 = CITY_NAMES[idx].to_string();
                    }
                }
                if let Some(c4) = comps.get_mut(4) {
                    if !c4.is_empty() {
                        *c4 = self.map_identifier("zip", c4);
                    }
                }
                comps.join(&comp_ch.to_string())
            })
            .collect();
        *val = new_reps.join(&rep_ch.to_string());
    }

    fn scrub_xtn_field(
        &self,
        fields: &mut [String],
        field_num: usize,
        delims: Delimiters,
        sweep: &mut BTreeMap<String, String>,
    ) {
        let Some(val) = fields.get_mut(field_num - 1) else {
            return;
        };
        if val.is_empty() {
            return;
        }
        let rep_ch = char::from(delims.repetition);
        let comp_ch = char::from(delims.component);

        let new_reps: Vec<String> = val
            .split(rep_ch)
            .map(|rep| {
                let mut comps: Vec<String> = rep.split(comp_ch).map(str::to_string).collect();
                if let Some(c0) = comps.get_mut(0) {
                    if !c0.is_empty() {
                        let orig = c0.clone();
                        let synth = self.map_phone(&orig);
                        if orig.chars().filter(char::is_ascii_digit).count() >= 7 {
                            record_sweep_token(sweep, &orig, &synth);
                        }
                        *c0 = synth;
                    }
                }
                if let Some(c3) = comps.get_mut(3) {
                    if !c3.is_empty() && c3.contains('@') {
                        let orig = c3.clone();
                        let synth = self.map_email(&orig);
                        record_sweep_token(sweep, &orig, &synth);
                        *c3 = synth;
                    }
                }
                if let Some(c6) = comps.get_mut(6) {
                    if !c6.is_empty() {
                        let orig = c6.clone();
                        let synth = self.map_phone(&orig);
                        if orig.chars().filter(char::is_ascii_digit).count() >= 7 {
                            record_sweep_token(sweep, &orig, &synth);
                        }
                        *c6 = synth;
                    }
                }
                comps.join(&comp_ch.to_string())
            })
            .collect();
        *val = new_reps.join(&rep_ch.to_string());
    }
}

struct Pass1Record {
    patient_key: Option<String>,
    sweep_tokens: BTreeMap<String, String>,
    scrubbed_segments: Vec<Vec<u8>>,
}

fn is_protected_hl7_code(token: &str) -> bool {
    matches!(
        token.to_ascii_uppercase().as_str(),
        "MSH"
            | "EVN"
            | "PID"
            | "PD1"
            | "PV1"
            | "PV2"
            | "NK1"
            | "OBR"
            | "OBX"
            | "ORC"
            | "NTE"
            | "AL1"
            | "DG1"
            | "PR1"
            | "GT1"
            | "IN1"
            | "IN2"
            | "ROL"
            | "SCH"
            | "FT1"
            | "TXA"
            | "MSA"
            | "ERR"
            | "FHS"
            | "BHS"
            | "BTS"
            | "FTS"
            | "ADT"
            | "ORU"
            | "ORM"
            | "SIU"
            | "MDM"
            | "DFT"
            | "ACK"
            | "BAR"
            | "VXU"
            | "RDE"
            | "OMG"
            | "OML"
            | "USA"
            | "CAN"
            | "PRN"
            | "WPN"
            | "NET"
            | "CPH"
            | "CWE"
            | "XPN"
            | "XAD"
            | "XTN"
            | "XCN"
    )
}

fn record_name_sweep_token(map: &mut BTreeMap<String, String>, orig: &str, synth: &str) {
    let trimmed = orig.trim();
    // Require len >= 4 for name/street tokens so 2-3 char names like "Lee" or "May"
    // never clobber HL7 coded values or short English words during whole-body sweeps.
    if trimmed.len() >= 4 && !is_protected_hl7_code(trimmed) && !trimmed.eq_ignore_ascii_case(synth)
    {
        map.entry(trimmed.to_string())
            .or_insert_with(|| synth.to_string());
    }
}

fn record_sweep_token(map: &mut BTreeMap<String, String>, orig: &str, synth: &str) {
    let trimmed = orig.trim();
    if trimmed.len() >= 3 && !is_protected_hl7_code(trimmed) && !trimmed.eq_ignore_ascii_case(synth)
    {
        map.entry(trimmed.to_string())
            .or_insert_with(|| synth.to_string());
    }
}

fn is_email_local_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'%' | b'+' | b'-')
}

fn is_email_domain_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-')
}

fn is_valid_email_domain(domain: &str) -> bool {
    let Some((host, tld)) = domain.rsplit_once('.') else {
        return false;
    };
    !host.is_empty()
        && !host.starts_with('.')
        && !host.ends_with('.')
        && !domain.contains("..")
        && (2..=24).contains(&tld.len())
        && tld.bytes().all(|b| b.is_ascii_alphabetic())
}

fn extract_patient_key(
    record: &Record,
    charset: &crate::encoding::ResolvedCharset,
) -> Option<String> {
    let delims = record.delimiters();
    for seg_idx in 0..record.segments().len() {
        let Some(seg) = record.segment_bytes(seg_idx) else {
            continue;
        };
        if seg.len() < 4 || &seg[0..3] != b"PID" || seg[3] != delims.field {
            continue;
        }
        let fields: Vec<&[u8]> = seg[4..].split(|&b| b == delims.field).collect();
        // Prefer PID-3.1, then PID-2.1, then PID-5.1
        for &f_idx in &[2usize, 1, 4] {
            if let Some(raw_f) = fields.get(f_idx) {
                let first_rep = raw_f
                    .split(|&b| b == delims.repetition)
                    .next()
                    .unwrap_or(b"");
                let first_comp = first_rep
                    .split(|&b| b == delims.component)
                    .next()
                    .unwrap_or(b"");
                let decoded = charset.decode_display(first_comp).trim().to_string();
                if !decoded.is_empty() {
                    return Some(decoded);
                }
            }
        }
    }
    None
}

fn join_segment(tag: &[u8; 3], field_sep: u8, fields: &[String]) -> Vec<u8> {
    let mut out = Vec::with_capacity(tag.len() + 1 + fields.len() * 8);
    out.extend_from_slice(tag);
    for f in fields {
        out.push(field_sep);
        out.extend_from_slice(f.as_bytes());
    }
    out
}

/// Replace all case-insensitive, token-boundary occurrences of `pairs` in `text`.
fn sweep_free_text(text: &str, pairs: &[(String, String)]) -> String {
    let mut current = text.to_string();
    for (orig, synth) in pairs {
        if orig.len() < 3 {
            continue;
        }
        current = replace_token_boundary_ci(&current, orig, synth);
    }
    current
}

fn replace_token_boundary_ci(haystack: &str, needle: &str, replacement: &str) -> String {
    let h_lower = haystack.to_ascii_lowercase();
    let n_lower = needle.to_ascii_lowercase();
    if n_lower.is_empty() || h_lower.len() < n_lower.len() {
        return haystack.to_string();
    }

    let mut out = String::with_capacity(haystack.len());
    let mut pos = 0usize;
    let mut last_copied = 0usize;

    while pos + n_lower.len() <= h_lower.len() {
        let Some(rel) = h_lower[pos..].find(&n_lower) else {
            break;
        };
        let start = pos + rel;
        let end = start + n_lower.len();
        let prev_ok = haystack[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        let next_ok = haystack[end..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        if prev_ok && next_ok {
            out.push_str(&haystack[last_copied..start]);
            out.push_str(replacement);
            last_copied = end;
            pos = end;
        } else {
            let step = haystack[start..].chars().next().map_or(1, char::len_utf8);
            pos = start + step;
        }
    }
    if last_copied == 0 {
        return haystack.to_string();
    }
    out.push_str(&haystack[last_copied..]);
    out
}

// --- Zero-dependency FIPS 180-4 SHA-256 & RFC 2104 HMAC-SHA256 ---

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut key_block = [0u8; 64];
    if key.len() > 64 {
        let hashed = sha256(key);
        key_block[..32].copy_from_slice(&hashed);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5Cu8; 64];
    for i in 0..64 {
        ipad[i] ^= key_block[i];
        opad[i] ^= key_block[i];
    }

    let mut inner = Vec::with_capacity(64 + data.len());
    inner.extend_from_slice(&ipad);
    inner.extend_from_slice(data);
    let inner_hash = sha256(&inner);

    let mut outer = [0u8; 96];
    outer[..64].copy_from_slice(&opad);
    outer[64..].copy_from_slice(&inner_hash);
    sha256(&outer)
}

fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity(data.len() + 72);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

const FAMILY_NAMES: &[&str] = &[
    "ADAMS", "BAKER", "BROOKS", "CAMPBELL", "CARTER", "CLARK", "COLLINS", "COOPER", "COX", "DAVIS",
    "DIAZ", "EDWARDS", "ELLIS", "EVANS", "FISHER", "FLORES", "FOSTER", "GARCIA", "GOMEZ", "GRAY",
    "GREEN", "HALL", "HARPER", "HARRIS", "HAYES", "HILL", "HOWARD", "HUGHES", "JAMES", "JENKINS",
    "KELLY", "KIM", "KING", "LEWIS", "LONG", "LOPEZ", "MARTIN", "MILLER", "MITCHELL", "MOORE",
    "MORGAN", "MORRIS", "MURPHY", "MYERS", "NELSON", "NGUYEN", "ORTIZ", "PARKER", "PATEL", "PEREZ",
    "PERRY", "PRICE", "RAMIREZ", "REED", "REYES", "RIVERA", "ROBERTS", "ROGERS", "ROSS", "SANDERS",
    "SCOTT", "STEWART", "TAYLOR", "TURNER",
];

const GIVEN_NAMES: &[&str] = &[
    "ALEX", "AMARA", "AVERY", "BLAKE", "BROOKE", "CALEB", "CAMERON", "CASEY", "CHLOE", "CLARA",
    "CONNOR", "DAKOTA", "DEVON", "DYLAN", "ELENA", "ELIAS", "EMERY", "ETHAN", "FINN", "GABRIEL",
    "GRACE", "HANNAH", "HARPER", "HAYDEN", "ISAAC", "IVY", "JADEN", "JAMIE", "JORDAN", "JULIA",
    "KAI", "KENDALL", "LIAM", "LOGAN", "LUCAS", "MAYA", "MICAH", "MORGAN", "NAOMI", "NOAH", "NORA",
    "OLIVER", "OWEN", "PAIGE", "PARKER", "QUINN", "REESE", "RILEY", "ROWAN", "RUBY", "SAMUEL",
    "SAWYER", "SOPHIA", "SPENCER", "SYDNEY", "TAYLOR", "TESSA", "THEO", "VALERIE", "VICTOR",
    "VIOLET", "WESLEY", "WILLOW", "ZOE",
];

const STREET_NAMES: &[&str] = &[
    "OAKWOOD LN",
    "MAPLE AVE",
    "CEDAR ST",
    "PINECREST DR",
    "BIRCHWOOD RD",
    "WILLOW WAY",
    "SYCAMORE BLVD",
    "ELMVIEW CT",
    "ASPEN PL",
    "CYPRESS TRL",
    "LAUREL AVE",
    "MAGNOLIA DR",
    "CHESTNUT ST",
    "HAWTHORN RD",
    "REDWOOD WAY",
    "SPRUCE LN",
];

const CITY_NAMES: &[&str] = &[
    "SPRINGFIELD",
    "RIVERTON",
    "FAIRVIEW",
    "OAKRIDGE",
    "LAKEWOOD",
    "GREENFIELD",
    "HILLCREST",
    "BROOKSIDE",
    "MEADOWBROOK",
    "WESTFIELD",
    "CLEARWATER",
    "MAPLEWOOD",
    "SUNNYVALE",
    "NORTHVIEW",
    "PINEHURST",
    "EASTWOOD",
];

const ORG_NAMES: &[&str] = &[
    "ACME HEALTH CORP",
    "NORTHWIND LOGISTICS",
    "CONTOSO MANUFACTURING",
    "FABRIKAM SERVICES",
    "TAILSPIN VENTURES",
    "ADVENTURE WORKS LLC",
    "WOODGROVE HOLDINGS",
    "BLUE YONDER GROUP",
    "ALPINE SKI HOUSE",
    "DATUM ENTERPRISES",
    "PROSEWARE INC",
    "LUCERNE PUBLISHING",
    "TREY RESEARCH",
    "LITWARE SYSTEMS",
    "BALDWIN INDUSTRIES",
    "COHO VINEYARDS",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::FieldAddress;
    use crate::io::load_from_bytes;
    use crate::scanner::ScannerConfig;

    #[test]
    fn referential_consistency_body_sweep_and_seed_determinism() {
        let raw = concat!(
            "MSH|^~\\&|EHR|HOSP|LAB|HOSP|20261123100000||ADT^A01|MSG1|P|2.5.1\r",
            "PID|1||MRN88421^^^HOSP^MR||THOMPSON^ROBERT^J||19790514|M|||742 EVERGREEN TER^^CHICAGO^IL^60611||312-555-9821|||M||ACC9001|123-45-6789\r",
            "PV1|1|I|MED^101^A||||100^DOC^JANE||||||||||||ENC77812\r",
            "NTE|1|L|Patient Robert Thompson (MRN88421) admitted under ENC77812 for routine glucose check.\r",
            "MSH|^~\\&|LAB|HOSP|EHR|HOSP|20261123110000||ORU^R01|MSG2|P|2.5.1\r",
            "PID|1||MRN88421^^^HOSP^MR||THOMPSON^ROBERT^J||19790514|M\r",
            "OBX|1|TX|NOTE||Result verified for THOMPSON, Robert (MRN88421); glucose normal.\r",
        );
        let records = load_from_bytes(raw.as_bytes(), ScannerConfig::default())
            .unwrap()
            .records;

        let scrubber_a1 = PhiScrubber::new("seed-alpha-2026");
        let scrubber_a2 = PhiScrubber::new("seed-alpha-2026");
        let scrubber_b = PhiScrubber::new("seed-beta-2026");

        let out_a1 = scrubber_a1.scrub_records(&records);
        let out_a2 = scrubber_a2.scrub_records(&records);
        let out_b = scrubber_b.scrub_records(&records);

        // Identical seeds produce byte-identical synthetic output
        assert_eq!(out_a1[0].bytes(), out_a2[0].bytes());
        assert_eq!(out_a1[1].bytes(), out_a2[1].bytes());

        // Different seeds produce different synthetic output
        assert_ne!(out_a1[0].bytes(), out_b[0].bytes());

        // 1. Referential consistency across ADT^A01 and ORU^R01
        let pid3_1: FieldAddress = "PID-3.1".parse().unwrap();
        let pid5_1: FieldAddress = "PID-5.1".parse().unwrap();
        let pid5_2: FieldAddress = "PID-5.2".parse().unwrap();
        let pid7: FieldAddress = "PID-7".parse().unwrap();
        let pid19: FieldAddress = "PID-19".parse().unwrap();
        let pv1_19: FieldAddress = "PV1-19".parse().unwrap();

        let synth_mrn_adt =
            std::str::from_utf8(out_a1[0].field_bytes(1, &pid3_1).unwrap()).unwrap();
        let synth_mrn_oru =
            std::str::from_utf8(out_a1[1].field_bytes(1, &pid3_1).unwrap()).unwrap();
        assert_eq!(synth_mrn_adt, synth_mrn_oru);
        assert_ne!(synth_mrn_adt, "MRN88421");
        assert_eq!(synth_mrn_adt.len(), "MRN88421".len());
        assert!(synth_mrn_adt[..3].chars().all(|c| c.is_ascii_uppercase()));
        assert!(synth_mrn_adt[3..].chars().all(|c| c.is_ascii_digit()));

        let synth_last_adt =
            std::str::from_utf8(out_a1[0].field_bytes(1, &pid5_1).unwrap()).unwrap();
        let synth_last_oru =
            std::str::from_utf8(out_a1[1].field_bytes(1, &pid5_1).unwrap()).unwrap();
        assert_eq!(synth_last_adt, synth_last_oru);
        assert_ne!(synth_last_adt, "THOMPSON");

        let synth_first_adt =
            std::str::from_utf8(out_a1[0].field_bytes(1, &pid5_2).unwrap()).unwrap();
        let synth_first_oru =
            std::str::from_utf8(out_a1[1].field_bytes(1, &pid5_2).unwrap()).unwrap();
        assert_eq!(synth_first_adt, synth_first_oru);
        assert_ne!(synth_first_adt, "ROBERT");

        let synth_dob_adt = std::str::from_utf8(out_a1[0].field_bytes(1, &pid7).unwrap()).unwrap();
        let synth_dob_oru = std::str::from_utf8(out_a1[1].field_bytes(1, &pid7).unwrap()).unwrap();
        assert_eq!(synth_dob_adt, synth_dob_oru);
        assert_ne!(synth_dob_adt, "19790514");
        assert_eq!(synth_dob_adt.len(), 8);

        let synth_ssn = std::str::from_utf8(out_a1[0].field_bytes(1, &pid19).unwrap()).unwrap();
        assert_ne!(synth_ssn, "123-45-6789");
        assert_eq!(synth_ssn.as_bytes()[3], b'-');
        assert_eq!(synth_ssn.as_bytes()[6], b'-');

        let synth_enc = std::str::from_utf8(out_a1[0].field_bytes(2, &pv1_19).unwrap()).unwrap();
        assert_ne!(synth_enc, "ENC77812");

        // 2. Body-wide free-text sweep in NTE-3 and OBX-5
        let nte3 =
            std::str::from_utf8(out_a1[0].field_bytes(3, &"NTE-3".parse().unwrap()).unwrap())
                .unwrap();
        assert!(
            !nte3.to_ascii_uppercase().contains("THOMPSON"),
            "NTE-3: {nte3}"
        );
        assert!(
            !nte3.to_ascii_uppercase().contains("ROBERT"),
            "NTE-3: {nte3}"
        );
        assert!(!nte3.contains("MRN88421"), "NTE-3: {nte3}");
        assert!(!nte3.contains("ENC77812"), "NTE-3: {nte3}");
        assert!(nte3.contains(synth_last_adt), "NTE-3: {nte3}");
        assert!(nte3.contains(synth_mrn_adt), "NTE-3: {nte3}");
        assert!(nte3.contains(synth_enc), "NTE-3: {nte3}");
        assert!(
            nte3.contains("glucose check"),
            "unrelated clinical text preserved: {nte3}"
        );

        let obx5 =
            std::str::from_utf8(out_a1[1].field_bytes(2, &"OBX-5".parse().unwrap()).unwrap())
                .unwrap();
        assert!(
            !obx5.to_ascii_uppercase().contains("THOMPSON"),
            "OBX-5: {obx5}"
        );
        assert!(
            !obx5.to_ascii_uppercase().contains("ROBERT"),
            "OBX-5: {obx5}"
        );
        assert!(!obx5.contains("MRN88421"), "OBX-5: {obx5}");
        assert!(obx5.contains(synth_last_oru), "OBX-5: {obx5}");
        assert!(obx5.contains(synth_mrn_oru), "OBX-5: {obx5}");
        assert!(
            obx5.contains("glucose normal"),
            "unrelated clinical text preserved: {obx5}"
        );

        // Segment headers untouched
        assert!(out_a1[0]
            .bytes()
            .starts_with(b"MSH|^~\\&|EHR|HOSP|LAB|HOSP|"));
    }

    #[test]
    fn whole_body_phi_and_email_sweep_and_cross_message_patient_pooling() {
        // Message 1 (ADT^A01): Full PID with name, MRN, DOB, street, phone, XTN.4 email,
        // plus arbitrary segments (DG1-4, SCH-7, PV2-12, OBR-13, NTE-3) repeating PHI and
        // a standalone email address not listed in PID-13.
        // Message 2 (ORU^R01): Shares the same PID-3 MRN (`MRN44210`) WITHOUT repeating the
        // patient's name, address, or email in PID, yet mentions them in OBR-13 and NTE-3.
        // Message 3 (ADT^A08): Patient with a short 3-char surname ("LEE") + coded values
        // ("ADT", "PID", "OBX", "M") to verify short names never clobber HL7 codes.
        let raw = concat!(
            "MSH|^~\\&|EHR|HOSP|LAB|HOSP|20261123100000||ADT^A01|WB1|P|2.5.1\r",
            "PID|1||MRN44210^^^HOSP^MR||MARTINEZ^CARLOS^R||19810322|M|||742 EVERGREEN TER^APT 2^SPRINGFIELD^IL^62704^USA||(312)555-8291^PRN^PH^carlos.martinez@hospital-patient.org|||M||ACC90112|555-12-3456\r",
            "PV2|||FOLLOWUP|||||||||Admit note for CARLOS MARTINEZ at 742 EVERGREEN TER\r",
            "DG1|1||J45.909^Asthma^I10|Asthma follow-up for MARTINEZ (DOB 1981-03-22, MRN44210)||A\r",
            "SCH|1001||||||Call (312)555-8291 or email carlos.martinez@hospital-patient.org\r",
            "OBR|1|ORD1|FIL1|CBC^Complete Blood Count||||||||Send copy to spouse.alt@clinic-mail.com\r",
            "NTE|1|L|SSN 555-12-3456 verified for CARLOS MARTINEZ\r",
            "MSH|^~\\&|LAB|HOSP|EHR|HOSP|20261123110000||ORU^R01|WB2|P|2.5.1\r",
            "PID|1||MRN44210^^^HOSP^MR\r",
            "OBR|1|ORD1|FIL1|CBC^Complete Blood Count||||||||Result for CARLOS MARTINEZ (carlos.martinez@hospital-patient.org) at 742 EVERGREEN TER\r",
            "NTE|1|L|Callback (312)555-8291 or spouse.alt@clinic-mail.com regarding MRN44210\r",
            "MSH|^~\\&|EHR|HOSP|LAB|HOSP|20261123120000||ADT^A08|WB3|P|2.5.1\r",
            "PID|1||MRN77119^^^HOSP^MR||LEE^MAY||19900105|F\r",
            "OBX|1|ST|NOTE||ADT patient PID OBX check in May\r",
        );

        let loaded = load_from_bytes(raw.as_bytes(), ScannerConfig::default()).unwrap();
        assert_eq!(loaded.records.len(), 3);

        let scrubber = PhiScrubber::new("whole-body-seed-99");
        let out = scrubber.scrub_records(&loaded.records);
        assert_eq!(out.len(), 3);

        let msg1 = std::str::from_utf8(out[0].bytes()).unwrap();
        let msg2 = std::str::from_utf8(out[1].bytes()).unwrap();
        let msg3 = std::str::from_utf8(out[2].bytes()).unwrap();

        // 1. All structured + body-wide PHI & emails scrubbed from Message 1
        for forbidden in [
            "MARTINEZ",
            "CARLOS",
            "MRN44210",
            "742 EVERGREEN TER",
            "(312)555-8291",
            "carlos.martinez@hospital-patient.org",
            "spouse.alt@clinic-mail.com",
            "555-12-3456",
            "1981-03-22",
            "19810322",
        ] {
            assert!(
                !msg1
                    .to_ascii_uppercase()
                    .contains(&forbidden.to_ascii_uppercase()),
                "Message 1 leaked '{forbidden}':\n{msg1}"
            );
        }
        assert!(
            msg1.contains("@example.invalid"),
            "expected RFC 2606 email in msg1:\n{msg1}"
        );
        // Segment tags and MSH-1/MSH-2 preserved intact
        assert!(msg1.starts_with("MSH|^~\\&|EHR|HOSP|LAB|HOSP|"));
        assert!(msg1.contains("\rPV2|") && msg1.contains("\rDG1|") && msg1.contains("\rSCH|"));

        // 2. Cross-message patient token pooling: Message 2 had only PID-3, yet OBR-13 and NTE-3
        //    must have CARLOS, MARTINEZ, email, street, phone, and MRN44210 scrubbed to the
        //    EXACT SAME synthetic values as Message 1!
        for forbidden in [
            "MARTINEZ",
            "CARLOS",
            "MRN44210",
            "742 EVERGREEN TER",
            "(312)555-8291",
            "carlos.martinez@hospital-patient.org",
            "spouse.alt@clinic-mail.com",
        ] {
            assert!(
                !msg2
                    .to_ascii_uppercase()
                    .contains(&forbidden.to_ascii_uppercase()),
                "Message 2 leaked '{forbidden}':\n{msg2}"
            );
        }
        let pid5_1: crate::FieldAddress = "PID-5.1".parse().unwrap();
        let synth_surname = std::str::from_utf8(out[0].field_bytes(1, &pid5_1).unwrap()).unwrap();
        let pid13_4: crate::FieldAddress = "PID-13.4".parse().unwrap();
        let synth_email = std::str::from_utf8(out[0].field_bytes(1, &pid13_4).unwrap()).unwrap();
        assert!(synth_email.ends_with("@example.invalid"));
        assert!(
            msg2.contains(synth_surname),
            "Message 2 should use pooled synthetic surname '{synth_surname}':\n{msg2}"
        );
        assert!(
            msg2.contains(synth_email),
            "Message 2 should use pooled synthetic email '{synth_email}':\n{msg2}"
        );

        // 3. Short 3-char surname ("LEE") / given name ("MAY") are scrubbed in PID-5
        //    but do NOT clobber HL7 coded values or short words in OBX-5.
        let pid5_1_m3 = std::str::from_utf8(out[2].field_bytes(1, &pid5_1).unwrap()).unwrap();
        assert_ne!(pid5_1_m3, "LEE");
        assert!(
            msg3.contains("ADT^A08")
                && msg3.contains("OBX|1|ST|NOTE||ADT patient PID OBX check in May"),
            "Message 3 should preserve HL7 codes and short words:\n{msg3}"
        );
    }

    #[test]
    fn clinical_scheduling_pharmacy_financial_and_flowsheet_guardrails() {
        let raw = concat!(
            // 1. ADT^A01 with full patient, PV1-19/50, GT1 (employer & spouse), IN1/IN2/IN3, ACC, BLG
            "MSH|^~\\&|EHR|HOSP|BILL|HOSP|20261123080000||ADT^A01|FIN1|P|2.5.1\r",
            "PID|1||MRN60011^^^HOSP^MR||SANCHEZ^ELENA^M||19840712|F|||450 MAPLE AVE^^AUSTIN^TX^78701||(512)555-3344^PRN^PH^elena.sanchez@patient-domain.org\r",
            "PV1|1|I|MED^201^A||||||||||||||||VIS60011|||||||||||||||||||||||||||||||ALTVIS60011\r",
            "GT1|1|GUAR60011|SANCHEZ^ELENA|SANCHEZ^MATEO|450 MAPLE AVE^^AUSTIN^TX^78701|(512)555-3344|(512)555-9988|19840712|||SELF|111-22-3333||||GLOBEX CORP|900 CONGRESS AVE^^AUSTIN^TX^78701|(512)555-7700\r",
            "IN1|1|PLAN998877|BCBS_TX|BLUE CROSS BLUE SHIELD|||||GLOBEX GROUP|EMP7766|GLOBEX CORP|||||SANCHEZ^ELENA||19840712|450 MAPLE AVE^^AUSTIN^TX^78701|||||||||||||||||POL887766|||||||||||||INSID4455\r",
            "IN2|EMP_IN2_1|111-22-3333||||MBI998877||MEDICAID_CASE_SANCHEZ|MCD554433||||||||||||||||||||||||||||||||||||||||||||||||||||MEM667788||(512)555-1212|(512)555-3434\r",
            "IN3|1|CERT778899\r",
            "ACC|20261122|AUTO|INTERSTATE 35 EXIT 234||||||||1200 HIGHWAY RD^^AUSTIN^TX^78702\r",
            "BLG||CH|BLGACCT9900\r",
            // 2. ORU^R01 for same MRN with ORC/OBR order/accession IDs, TXA doc IDs, flowsheet OBX-3 + numeric/coded OBX-5 + free-text OBX-5
            "MSH|^~\\&|LAB|HOSP|EHR|HOSP|20261123090000||ORU^R01|FIN2|P|2.5.1\r",
            "PID|1||MRN60011^^^HOSP^MR\r",
            "ORC|RE|PLACER1001^EHR|FILLER2002^LAB|CASE3003^EHR\r",
            "OBR|1|PLACER1001^EHR|FILLER2002^LAB|8867-4^Heart Rate^LN\r",
            "OBX|1|NM|8867-4^Heart Rate^LN||72|/min\r",
            "OBX|2|CWE|72166-2^Tobacco Status^LN||LA18978-9^Never smoker^LN\r",
            "OBX|3|TX|NOTE||Order PLACER1001 / FILLER2002 for ELENA SANCHEZ (elena.sanchez@patient-domain.org) at GLOBEX CORP\r",
            "TXA|1|DS|TX|||||||||DOC990011|PARENTDOC8800\r",
            // 3. DFT^P03 + SIU^S12 + RDE^O11 for same MRN
            "MSH|^~\\&|BILL|HOSP|REV|HOSP|20261123100000||DFT^P03|FIN3|P|2.5.1\r",
            "PID|1||MRN60011^^^HOSP^MR\r",
            "FT1|1|TXN445566|BATCH778899|20261123||CG|99213^Office Visit^CPT|||1|150.00|150.00|||||||J06.9^URI^I10||||||99213\r",
            "SCH|APPT1122|FAPPT3344|||SCHED5566||||||||(512)555-6600^PRN^PH^sched.contact@clinic.org||||(512)555-6611\r",
            "RXE|1^BID|00093-3109-05^Amoxicillin 500mg^NDC|500||MG||||||||||RXNUM889900\r",
            "RXD|1|00093-3109-05^Amoxicillin 500mg^NDC|20261123|30|CAP||RXNUM889900\r",
            "NTE|1|L|Billed TXN445566 for ELENA SANCHEZ (POL887766, APPT1122, RXNUM889900)\r",
        );

        let loaded = load_from_bytes(raw.as_bytes(), ScannerConfig::default()).unwrap();
        assert_eq!(loaded.records.len(), 3);

        let scrubber = PhiScrubber::new("comprehensive-seed-2026");
        let out = scrubber.scrub_records(&loaded.records);

        let msg1 = std::str::from_utf8(out[0].bytes()).unwrap();
        let msg2 = std::str::from_utf8(out[1].bytes()).unwrap();
        let msg3 = std::str::from_utf8(out[2].bytes()).unwrap();
        let all_text = format!("{msg1}\n{msg2}\n{msg3}");

        // Every targeted PHI token must be absent across all 3 messages
        for forbidden in [
            "MRN60011",
            "SANCHEZ",
            "ELENA",
            "MATEO",
            "VIS60011",
            "ALTVIS60011",
            "GUAR60011",
            "111-22-3333",
            "GLOBEX CORP",
            "GLOBEX GROUP",
            "PLAN998877",
            "EMP7766",
            "POL887766",
            "INSID4455",
            "EMP_IN2_1",
            "MBI998877",
            "MCD554433",
            "MEM667788",
            "CERT778899",
            "INTERSTATE 35 EXIT 234",
            "1200 HIGHWAY RD",
            "BLGACCT9900",
            "PLACER1001",
            "FILLER2002",
            "CASE3003",
            "DOC990011",
            "PARENTDOC8800",
            "TXN445566",
            "BATCH778899",
            "APPT1122",
            "FAPPT3344",
            "SCHED5566",
            "RXNUM889900",
            "elena.sanchez@patient-domain.org",
            "sched.contact@clinic.org",
        ] {
            assert!(
                !all_text
                    .to_ascii_uppercase()
                    .contains(&forbidden.to_ascii_uppercase()),
                "PHI token '{forbidden}' leaked in scrubbed output:\n{all_text}"
            );
        }

        // Referential consistency: ORC-2 == OBR-2, ORC-3 == OBR-3, RXE-15 == RXD-7
        let orc2 = std::str::from_utf8(out[1].field_bytes(2, &"ORC-2.1".parse().unwrap()).unwrap())
            .unwrap();
        let obr2 = std::str::from_utf8(out[1].field_bytes(3, &"OBR-2.1".parse().unwrap()).unwrap())
            .unwrap();
        assert_eq!(orc2, obr2, "ORC-2 and OBR-2 must map identically");
        let rxe15 = std::str::from_utf8(out[2].field_bytes(4, &"RXE-15".parse().unwrap()).unwrap())
            .unwrap();
        let rxd7 =
            std::str::from_utf8(out[2].field_bytes(5, &"RXD-7".parse().unwrap()).unwrap()).unwrap();
        assert_eq!(rxe15, rxd7, "RXE-15 and RXD-7 must map identically");

        // Clinical, payer, pharmacy, financial, and flowsheet guardrails MUST be preserved
        assert!(
            msg1.contains("|BCBS_TX|BLUE CROSS BLUE SHIELD|"),
            "IN1-3/4 payer preserved"
        );
        assert!(
            msg2.contains("OBX|1|NM|8867-4^Heart Rate^LN||72|/min"),
            "OBX-3 flowsheet ID and numeric OBX-5 preserved:\n{msg2}"
        );
        assert!(
            msg2.contains("OBX|2|CWE|72166-2^Tobacco Status^LN||LA18978-9^Never smoker^LN"),
            "Coded CWE OBX-5 preserved:\n{msg2}"
        );
        assert!(
            msg3.contains(
                "|CG|99213^Office Visit^CPT|||1|150.00|150.00|||||||J06.9^URI^I10||||||99213"
            ),
            "FT1 CPT/amounts/diagnosis codes preserved:\n{msg3}"
        );
        assert!(
            msg3.contains("|00093-3109-05^Amoxicillin 500mg^NDC|"),
            "RXE-2 / RXD-2 NDC medication codes preserved:\n{msg3}"
        );
    }

    #[test]
    fn custom_extra_phi_paths_scrub_z_segments_components_and_repeating_segments() {
        let raw = concat!(
            "MSH|^~\\&|EHR|HOSP|LAB|HOSP|20261124090000||ORU^R01|CUST1|P|2.5.1\r",
            "PID|1||MRN5500^^^HOSP^MR||DOE^JANE||19800101|F||||||||||ACCT-7788^CHECKDIGIT\r",
            "OBX|1|TX|NOTE||Confidential note alpha\r",
            "OBX|2|TX|NOTE||Confidential note beta\r",
            "ZPI|1|KEEP_ME|ALSO_KEEP|SITE_SECRET_9911~SITE_SECRET_9922\r",
        );
        let records = load_from_bytes(raw.as_bytes(), ScannerConfig::default())
            .unwrap()
            .records;

        let rules = vec![
            CustomPhiRule {
                path: "ZPI-4".to_string(),
                mode: CustomPhiMode::Synthetic,
            },
            CustomPhiRule {
                path: "PID-18.1".to_string(),
                mode: CustomPhiMode::Mask,
            },
            CustomPhiRule {
                path: "OBX-5".to_string(),
                mode: CustomPhiMode::Redact,
            },
        ];

        // Live match count: 2 repetitions in ZPI-4 + 1 in PID-18.1 + 2 repeating OBX-5 = 5
        assert_eq!(count_custom_phi_matches(&records, &rules), 5);

        let scrubber = PhiScrubber::new("custom-seed-42").with_extra_paths(rules);
        let out = scrubber.scrub_records(&records);
        let text = String::from_utf8_lossy(out[0].bytes());

        // ZPI-4 repetitions replaced deterministically with SYN_..., while ZPI-2/3 untouched
        assert!(!text.contains("SITE_SECRET_9911"));
        assert!(!text.contains("SITE_SECRET_9922"));
        assert!(text.contains("ZPI|1|KEEP_ME|ALSO_KEEP|SYN_"));

        // PID-18.1 masked with '*' while PID-18.2 (`CHECKDIGIT`) is preserved
        assert!(!text.contains("ACCT-7788"));
        assert!(
            text.contains("*^CHECKDIGIT"),
            "expected PID-18.1 masked and PID-18.2 preserved:\n{text}"
        );

        // Both repeating OBX-5 segments redacted
        assert!(!text.contains("Confidential note alpha"));
        assert!(!text.contains("Confidential note beta"));
        assert_eq!(text.matches("[REDACTED]").count(), 2);
    }
}
