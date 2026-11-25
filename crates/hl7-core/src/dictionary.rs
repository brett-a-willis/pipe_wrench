//! Built-in HL7 v2 segment and field description dictionary with user Z-segment overlay (§5.4).

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::address::FieldAddress;
use crate::error::CoreError;

const BUILTIN_DICTIONARY_TOML: &str = include_str!("../../../dictionary/hl7v2.toml");

/// Description entry for a single HL7 segment and its numbered fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentDictionaryEntry {
    /// Human-readable segment description (e.g. `"Patient Identification"`).
    pub description: String,
    /// Map from 1-based field number string (`"1"`, `"2"`, ...) to field description.
    #[serde(default)]
    pub fields: HashMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RawDictionaryFile {
    #[serde(default)]
    segments: HashMap<String, SegmentDictionaryEntry>,
}

/// Lookup table of HL7 segment and field descriptions (§5.4).
#[derive(Debug, Clone)]
pub struct Hl7Dictionary {
    segments: HashMap<String, SegmentDictionaryEntry>,
}

impl Default for Hl7Dictionary {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Hl7Dictionary {
    /// Load the built-in `dictionary/hl7v2.toml` dictionary (§5.4).
    pub fn builtin() -> Self {
        let mut dict = Self {
            segments: HashMap::new(),
        };
        let _ = dict.merge_toml_str(BUILTIN_DICTIONARY_TOML);
        dict
    }

    /// Overlay segment and field descriptions from a TOML string (e.g. user Z-segments, §5.4).
    pub fn merge_toml_str(&mut self, toml_str: &str) -> Result<(), CoreError> {
        let parsed: RawDictionaryFile =
            toml::from_str(toml_str).map_err(|e| CoreError::Dictionary(e.to_string()))?;
        for (tag, entry) in parsed.segments {
            let key = tag.trim().to_ascii_uppercase();
            let target = self.segments.entry(key).or_default();
            if !entry.description.is_empty() {
                target.description = entry.description;
            }
            for (field_num, desc) in entry.fields {
                target.fields.insert(field_num, desc);
            }
        }
        Ok(())
    }

    /// Overlay segment and field descriptions from a user TOML dictionary file on disk (§5.4).
    pub fn merge_file(&mut self, path: &Path) -> Result<(), CoreError> {
        let content =
            std::fs::read_to_string(path).map_err(|e| CoreError::Dictionary(e.to_string()))?;
        self.merge_toml_str(&content)
    }

    /// Look up the description of a 3-byte segment tag (e.g. `b"PID"` -> `"Patient Identification"`).
    pub fn segment_description(&self, tag: &[u8; 3]) -> Option<&str> {
        let key = std::str::from_utf8(tag).ok()?;
        self.segments
            .get(key)
            .map(|s| s.description.as_str())
            .filter(|d| !d.is_empty())
    }

    /// Return all sorted 1-based field numbers defined in the dictionary for `tag`.
    pub fn segment_field_numbers(&self, tag: &[u8; 3]) -> Vec<u16> {
        let Ok(key) = std::str::from_utf8(tag) else {
            return Vec::new();
        };
        let Some(seg) = self.segments.get(key) else {
            return Vec::new();
        };
        let mut nums: Vec<u16> = seg
            .fields
            .keys()
            .filter_map(|k| k.parse::<u16>().ok())
            .filter(|&n| n > 0)
            .collect();
        nums.sort_unstable();
        nums
    }

    /// Look up the description of a field by segment tag and 1-based field index.
    pub fn field_description_by_num(&self, tag: &[u8; 3], field_num: u16) -> Option<&str> {
        let key = std::str::from_utf8(tag).ok()?;
        let seg = self.segments.get(key)?;
        seg.fields
            .get(&field_num.to_string())
            .map(String::as_str)
            .filter(|d| !d.is_empty())
    }

    /// Look up the human-readable description for a [`FieldAddress`] (§5.2, §5.4).
    ///
    /// - Top-level fields (`PID-5`, `MSH-9`, `OBR-16`) return the field description.
    /// - Component addresses (`PID-5.1`, `MSH-9.2`, `ORC-7.4`) return the HL7 v2 data type
    ///   component name (falling back to the parent field description if primitive/unmapped).
    /// - Subcomponent addresses (`PID-3.4.1`, `OBR-16.9.1`) return the nested subcomponent name
    ///   (falling back cleanly to component or parent field description).
    pub fn field_description(&self, addr: &FieldAddress) -> Option<&str> {
        let field_num = addr.field.get();
        if let Some(comp) = addr.component {
            if let Some(dt) = field_data_type(&addr.segment, field_num) {
                if let Some(comp_def) = data_type_component(dt, comp.get()) {
                    if let Some(sub) = addr.subcomponent {
                        if let Some(sub_dt) = comp_def.sub_type {
                            if let Some(sub_def) = data_type_component(sub_dt, sub.get()) {
                                return Some(sub_def.name);
                            }
                        }
                    }
                    return Some(comp_def.name);
                }
            }
        }
        self.field_description_by_num(&addr.segment, field_num)
    }

    /// Build a full hierarchical tooltip string for a [`FieldAddress`], e.g.
    /// `"PID-5.1 — Patient Name › Family Name (XPN.1)"` or
    /// `"PID-3.4.1 — Patient Identifier List › Assigning Authority › Namespace ID (CX.4.1 / HD.1)"`.
    pub fn field_tooltip(&self, addr: &FieldAddress) -> Option<String> {
        let field_num = addr.field.get();
        let field_name = self.field_description_by_num(&addr.segment, field_num)?;
        let dt = field_data_type(&addr.segment, field_num);

        match (addr.component, addr.subcomponent, dt) {
            (Some(comp), Some(sub), Some(dt_name)) => {
                if let Some(comp_def) = data_type_component(dt_name, comp.get()) {
                    if let Some(sub_dt) = comp_def.sub_type {
                        if let Some(sub_def) = data_type_component(sub_dt, sub.get()) {
                            return Some(format!(
                                "{addr} — {field_name} › {} › {} ({dt_name}.{}.{} / {sub_dt}.{})",
                                comp_def.name,
                                sub_def.name,
                                comp.get(),
                                sub.get(),
                                sub.get()
                            ));
                        }
                    }
                    return Some(format!(
                        "{addr} — {field_name} › {} ({dt_name}.{})",
                        comp_def.name,
                        comp.get()
                    ));
                }
                Some(format!("{addr} — {field_name} ({dt_name})"))
            }
            (Some(comp), None, Some(dt_name)) => {
                if let Some(comp_def) = data_type_component(dt_name, comp.get()) {
                    return Some(format!(
                        "{addr} — {field_name} › {} ({dt_name}.{})",
                        comp_def.name,
                        comp.get()
                    ));
                }
                Some(format!("{addr} — {field_name} ({dt_name})"))
            }
            (_, _, Some(dt_name)) => Some(format!("{addr} — {field_name} ({dt_name})")),
            _ => Some(format!("{addr} — {field_name}")),
        }
    }
}

/// Definition of a single 1-based component inside an HL7 v2 composite data type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataTypeComponent {
    /// 1-based component index.
    pub index: u16,
    /// Standard HL7 v2.5.1/v2.8 component name.
    pub name: &'static str,
    /// Optional nested composite data type for subcomponent resolution (e.g. `Some("HD")`, `Some("FN")`).
    pub sub_type: Option<&'static str>,
}

const fn dtc(index: u16, name: &'static str, sub_type: Option<&'static str>) -> DataTypeComponent {
    DataTypeComponent {
        index,
        name,
        sub_type,
    }
}

/// Look up the 1-based component definition for a standard HL7 v2 composite data type.
pub fn data_type_component(data_type: &str, comp: u16) -> Option<DataTypeComponent> {
    let table: &[DataTypeComponent] = match data_type {
        "MSG" => &[
            dtc(1, "Message Code", None),
            dtc(2, "Trigger Event", None),
            dtc(3, "Message Structure", None),
        ],
        "FN" => &[
            dtc(1, "Surname", None),
            dtc(2, "Own Surname Prefix", None),
            dtc(3, "Own Surname", None),
            dtc(4, "Surname Prefix From Partner/Spouse", None),
            dtc(5, "Surname From Partner/Spouse", None),
        ],
        "SAD" => &[
            dtc(1, "Street or Mailing Address", None),
            dtc(2, "Street Name", None),
            dtc(3, "Dwelling Number", None),
        ],
        "HD" => &[
            dtc(1, "Namespace ID", None),
            dtc(2, "Universal ID", None),
            dtc(3, "Universal ID Type", None),
        ],
        "PT" => &[
            dtc(1, "Processing ID", None),
            dtc(2, "Processing Mode", None),
        ],
        "VID" => &[
            dtc(1, "Version ID", None),
            dtc(2, "Internationalization Code", Some("CE")),
            dtc(3, "International Version ID", Some("CE")),
        ],
        "TS" | "DTM" => &[dtc(1, "Time", None), dtc(2, "Degree of Precision", None)],
        "XPN" => &[
            dtc(1, "Family Name", Some("FN")),
            dtc(2, "Given Name", None),
            dtc(
                3,
                "Second and Further Given Names or Initials Thereof",
                None,
            ),
            dtc(4, "Suffix (e.g., JR or III)", None),
            dtc(5, "Prefix (e.g., DR)", None),
            dtc(6, "Degree (e.g., MD)", None),
            dtc(7, "Name Type Code", None),
            dtc(8, "Name Representation Code", None),
            dtc(9, "Name Context", Some("CE")),
            dtc(10, "Name Validity Range", Some("DR")),
            dtc(11, "Name Assembly Order", None),
        ],
        "XCN" => &[
            dtc(1, "ID Number", None),
            dtc(2, "Family Name", Some("FN")),
            dtc(3, "Given Name", None),
            dtc(4, "Second and Further Given Names or Initials", None),
            dtc(5, "Suffix", None),
            dtc(6, "Prefix", None),
            dtc(7, "Degree", None),
            dtc(8, "Source Table", None),
            dtc(9, "Assigning Authority", Some("HD")),
            dtc(10, "Name Type Code", None),
            dtc(11, "Identifier Check Digit", None),
            dtc(12, "Check Digit Scheme", None),
            dtc(13, "Identifier Type Code", None),
            dtc(14, "Assigning Facility", Some("HD")),
            dtc(15, "Name Representation Code", None),
        ],
        "CX" => &[
            dtc(1, "ID Number", None),
            dtc(2, "Check Digit", None),
            dtc(3, "Check Digit Scheme", None),
            dtc(4, "Assigning Authority", Some("HD")),
            dtc(5, "Identifier Type Code", None),
            dtc(6, "Assigning Facility", Some("HD")),
            dtc(7, "Effective Date", None),
            dtc(8, "Expiration Date", None),
        ],
        "EI" => &[
            dtc(1, "Entity Identifier", None),
            dtc(2, "Namespace ID", None),
            dtc(3, "Universal ID", None),
            dtc(4, "Universal ID Type", None),
        ],
        "EIP" => &[
            dtc(1, "Placer Assigned Identifier", Some("EI")),
            dtc(2, "Filler Assigned Identifier", Some("EI")),
        ],
        "CE" | "CWE" | "CNE" => &[
            dtc(1, "Identifier", None),
            dtc(2, "Text", None),
            dtc(3, "Name of Coding System", None),
            dtc(4, "Alternate Identifier", None),
            dtc(5, "Alternate Text", None),
            dtc(6, "Name of Alternate Coding System", None),
            dtc(7, "Coding System Version ID", None),
            dtc(8, "Alternate Coding System Version ID", None),
            dtc(9, "Original Text", None),
        ],
        "CQ" => &[dtc(1, "Quantity", None), dtc(2, "Units", Some("CE"))],
        "TQ" => &[
            dtc(1, "Quantity", Some("CQ")),
            dtc(2, "Interval", Some("RI")),
            dtc(3, "Duration", None),
            dtc(4, "Start Date/Time", Some("TS")),
            dtc(5, "End Date/Time", Some("TS")),
            dtc(6, "Priority", None),
            dtc(7, "Condition", None),
            dtc(8, "Text", None),
            dtc(9, "Conjunction", None),
            dtc(10, "Order Sequencing", None),
            dtc(11, "Occurrence Duration", Some("CE")),
            dtc(12, "Total Occurrences", None),
        ],
        "PL" => &[
            dtc(1, "Point of Care", None),
            dtc(2, "Room", None),
            dtc(3, "Bed", None),
            dtc(4, "Facility", Some("HD")),
            dtc(5, "Location Status", None),
            dtc(6, "Person Location Type", None),
            dtc(7, "Building", None),
            dtc(8, "Floor", None),
            dtc(9, "Location Description", None),
        ],
        "XAD" => &[
            dtc(1, "Street Address", Some("SAD")),
            dtc(2, "Other Designation", None),
            dtc(3, "City", None),
            dtc(4, "State or Province", None),
            dtc(5, "Zip or Postal Code", None),
            dtc(6, "Country", None),
            dtc(7, "Address Type", None),
            dtc(8, "Other Geographic Designation", None),
            dtc(9, "County/Parish Code", None),
        ],
        "XTN" => &[
            dtc(1, "Telephone Number", None),
            dtc(2, "Telecommunication Use Code", None),
            dtc(3, "Telecommunication Equipment Type", None),
            dtc(4, "Email Address", None),
            dtc(5, "Country Code", None),
            dtc(6, "Area/City Code", None),
            dtc(7, "Local Number", None),
            dtc(8, "Extension", None),
            dtc(9, "Any Text", None),
            dtc(12, "Unformatted Telephone Number", None),
        ],
        "SPS" => &[
            dtc(1, "Specimen Source Name or Code", Some("CWE")),
            dtc(2, "Additives", Some("CWE")),
            dtc(3, "Specimen Collection Method", None),
            dtc(4, "Body Site", Some("CWE")),
            dtc(5, "Site Modifier", Some("CWE")),
            dtc(6, "Collection Method Modifier Code", Some("CWE")),
            dtc(7, "Specimen Role", Some("CWE")),
        ],
        "NDL" | "CNN" => &[
            dtc(1, "ID Number", None),
            dtc(2, "Family Name", Some("FN")),
            dtc(3, "Given Name", None),
            dtc(4, "Second/Middle Names", None),
            dtc(5, "Suffix", None),
            dtc(6, "Prefix", None),
            dtc(7, "Degree", None),
            dtc(8, "Source Table", None),
            dtc(9, "Assigning Authority", Some("HD")),
        ],
        "XON" => &[
            dtc(1, "Organization Name", None),
            dtc(2, "Organization Name Type Code", None),
            dtc(3, "ID Number", None),
            dtc(6, "Assigning Authority", Some("HD")),
            dtc(7, "Identifier Type Code", None),
            dtc(10, "Organization Identifier", None),
        ],
        "PRL" => &[
            dtc(1, "Parent Observation Identifier", Some("CE")),
            dtc(2, "Parent Observation Sub-identifier", None),
            dtc(3, "Parent Observation Value Descriptor", None),
        ],
        "RPT" => &[
            dtc(1, "Repeat Pattern Code", Some("CWE")),
            dtc(2, "Calendar Alignment", None),
            dtc(3, "Phase Range Begin Value", None),
            dtc(4, "Phase Range End Value", None),
            dtc(5, "Period Quantity", None),
            dtc(6, "Period Units", None),
            dtc(7, "Institution Specified Time", None),
            dtc(8, "Event", None),
            dtc(9, "Event Offset Quantity", None),
            dtc(10, "Event Offset Units", None),
            dtc(11, "General Timing Specification", None),
        ],
        "RI" => &[
            dtc(1, "Repeat Pattern", None),
            dtc(2, "Explicit Time Interval", None),
        ],
        "MO" => &[dtc(1, "Quantity", None), dtc(2, "Denomination", None)],
        "CP" => &[
            dtc(1, "Price", Some("MO")),
            dtc(2, "Price Type", None),
            dtc(3, "From Value", None),
            dtc(4, "To Value", None),
            dtc(5, "Range Units", Some("CE")),
            dtc(6, "Range Type", None),
        ],
        "MOC" => &[
            dtc(1, "Monetary Amount", Some("MO")),
            dtc(2, "Charge Code", Some("CE")),
        ],
        "CCD" => &[
            dtc(1, "Invocation Event", None),
            dtc(2, "Date/Time", Some("TS")),
        ],
        "SN" => &[
            dtc(1, "Comparator", None),
            dtc(2, "Num1", None),
            dtc(3, "Separator/Suffix", None),
            dtc(4, "Num2", None),
        ],
        "DR" => &[
            dtc(1, "Range Start Date/Time", Some("TS")),
            dtc(2, "Range End Date/Time", Some("TS")),
        ],
        "WVI" => &[dtc(1, "Channel Number", None), dtc(2, "Channel Name", None)],
        "WVS" => &[
            dtc(1, "Source One Name", None),
            dtc(2, "Source Two Name", None),
        ],
        "ED" => &[
            dtc(1, "Source Application", Some("HD")),
            dtc(2, "Type of Data", None),
            dtc(3, "Data Subtype", None),
            dtc(4, "Encoding", None),
            dtc(5, "Data", None),
        ],
        "RP" => &[
            dtc(1, "Pointer", None),
            dtc(2, "Application ID", Some("HD")),
            dtc(3, "Type of Data", None),
            dtc(4, "Subtype", None),
        ],
        "AD" => &[
            dtc(1, "Street Address", None),
            dtc(2, "Other Designation", None),
            dtc(3, "City", None),
            dtc(4, "State or Province", None),
            dtc(5, "Zip or Postal Code", None),
            dtc(6, "Country", None),
            dtc(7, "Address Type", None),
            dtc(8, "Other Geographic Designation", None),
        ],
        "LA1" => &[
            dtc(1, "Point of Care", None),
            dtc(2, "Room", None),
            dtc(3, "Bed", None),
            dtc(4, "Facility", Some("HD")),
            dtc(5, "Location Status", None),
            dtc(6, "Patient Location Type", None),
            dtc(7, "Building", None),
            dtc(8, "Floor", None),
            dtc(9, "Address", Some("AD")),
        ],
        "LA2" => &[
            dtc(1, "Point of Care", None),
            dtc(2, "Room", None),
            dtc(3, "Bed", None),
            dtc(4, "Facility", Some("HD")),
            dtc(5, "Location Status", None),
            dtc(6, "Patient Location Type", None),
            dtc(7, "Building", None),
            dtc(8, "Floor", None),
            dtc(9, "Street Address", None),
            dtc(10, "Other Designation", None),
            dtc(11, "City", None),
            dtc(12, "State or Province", None),
            dtc(13, "Zip or Postal Code", None),
            dtc(14, "Country", None),
            dtc(15, "Address Type", None),
            dtc(16, "Other Geographic Designation", None),
        ],
        "PPN" => &[
            dtc(1, "ID Number", None),
            dtc(2, "Family Name", Some("FN")),
            dtc(3, "Given Name", None),
            dtc(4, "Second and Further Given Names or Initials", None),
            dtc(5, "Suffix", None),
            dtc(6, "Prefix", None),
            dtc(7, "Degree", None),
            dtc(8, "Source Table", None),
            dtc(9, "Assigning Authority", Some("HD")),
            dtc(10, "Name Type Code", None),
            dtc(11, "Identifier Check Digit", None),
            dtc(12, "Check Digit Scheme", None),
            dtc(13, "Identifier Type Code", None),
            dtc(14, "Assigning Facility", Some("HD")),
            dtc(15, "Date/Time Action Performed", Some("TS")),
            dtc(16, "Name Representation Code", None),
        ],
        "PLN" => &[
            dtc(1, "ID Number", None),
            dtc(2, "Type of ID Number", None),
            dtc(3, "State/Other Qualifying Information", None),
            dtc(4, "Expiration Date", None),
        ],
        "DLD" => &[
            dtc(1, "Discharge Location", None),
            dtc(2, "Effective Date", Some("TS")),
        ],
        "FC" => &[
            dtc(1, "Financial Class Code", None),
            dtc(2, "Effective Date", Some("TS")),
        ],
        "JCC" => &[
            dtc(1, "Job Code", None),
            dtc(2, "Job Class", None),
            dtc(3, "Job Description Text", None),
        ],
        "CF" => &[
            dtc(1, "Identifier", None),
            dtc(2, "Formatted Text", None),
            dtc(3, "Name of Coding System", None),
            dtc(4, "Alternate Identifier", None),
            dtc(5, "Alternate Formatted Text", None),
            dtc(6, "Name of Alternate Coding System", None),
        ],
        "ELD" => &[
            dtc(1, "Segment ID", None),
            dtc(2, "Segment Sequence", None),
            dtc(3, "Field Position", None),
            dtc(4, "Code Identifying Error", Some("CE")),
        ],
        "ERL" => &[
            dtc(1, "Segment ID", None),
            dtc(2, "Segment Sequence", None),
            dtc(3, "Field Position", None),
            dtc(4, "Field Repetition", None),
            dtc(5, "Component Number", None),
            dtc(6, "Sub-Component Number", None),
        ],
        "DLN" => &[
            dtc(1, "License Number", None),
            dtc(2, "Issuing State, Province, Country", None),
            dtc(3, "Expiration Date", None),
        ],
        "AUI" => &[
            dtc(1, "Authorization Number", None),
            dtc(2, "Date", None),
            dtc(3, "Source", None),
        ],
        "SRT" => &[dtc(1, "Sort-by Field", None), dtc(2, "Sequencing", None)],
        _ => return None,
    };
    table.iter().copied().find(|c| c.index == comp)
}

/// Resolve the standard HL7 v2 composite data type for `(segment_tag, 1-based field_num)`.
pub fn field_data_type(tag: &[u8; 3], field_num: u16) -> Option<&'static str> {
    match (tag, field_num) {
        // MSH
        (b"MSH", 3..=6) => Some("HD"),
        (b"MSH", 7) => Some("TS"),
        (b"MSH", 9) => Some("MSG"),
        (b"MSH", 11) => Some("PT"),
        (b"MSH", 12) => Some("VID"),
        (b"MSH", 19) => Some("CE"),
        (b"MSH", 21) => Some("EI"),
        // FHS / BHS
        (b"FHS" | b"BHS", 3..=6) => Some("HD"),
        (b"FHS" | b"BHS", 7) => Some("TS"),
        // EVN
        (b"EVN", 2 | 3 | 6) => Some("TS"),
        (b"EVN", 5) => Some("XCN"),
        (b"EVN", 7) => Some("HD"),
        // PID
        (b"PID", 2 | 3 | 4 | 18 | 21) => Some("CX"),
        (b"PID", 5 | 6 | 9) => Some("XPN"),
        (b"PID", 7 | 29) => Some("TS"),
        (b"PID", 10 | 15 | 16 | 17 | 22 | 26 | 27 | 28) => Some("CE"),
        (b"PID", 11) => Some("XAD"),
        (b"PID", 13 | 14) => Some("XTN"),
        (b"PID", 20) => Some("DLN"),
        // PD1
        (b"PD1", 3) => Some("XON"),
        (b"PD1", 4) => Some("XCN"),
        (b"PD1", 11) => Some("CE"),
        // NK1
        (b"NK1", 2) => Some("XPN"),
        (b"NK1", 3 | 7) => Some("CE"),
        (b"NK1", 4) => Some("XAD"),
        (b"NK1", 5 | 6) => Some("XTN"),
        (b"NK1", 11) => Some("JCC"),
        (b"NK1", 13) => Some("XON"),
        // PV1
        (b"PV1", 3 | 6 | 11 | 42 | 43) => Some("PL"),
        (b"PV1", 5 | 19 | 50) => Some("CX"),
        (b"PV1", 7 | 8 | 9 | 17) => Some("XCN"),
        (b"PV1", 20) => Some("FC"),
        (b"PV1", 36 | 37) => Some("DLD"),
        (b"PV1", 44 | 45) => Some("TS"),
        // PV2
        (b"PV2", 1) => Some("PL"),
        (b"PV2", 2..=4) => Some("CE"),
        (b"PV2", 8 | 9) => Some("TS"),
        // ORC
        (b"ORC", 2..=4) => Some("EI"),
        (b"ORC", 7) => Some("TQ"),
        (b"ORC", 8) => Some("EIP"),
        (b"ORC", 9 | 15 | 27) => Some("TS"),
        (b"ORC", 10 | 11 | 12 | 19) => Some("XCN"),
        (b"ORC", 13) => Some("PL"),
        (b"ORC", 14 | 23) => Some("XTN"),
        (b"ORC", 16..=18 | 20 | 25 | 26 | 28 | 29 | 31) => Some("CWE"),
        (b"ORC", 21) => Some("XON"),
        (b"ORC", 22 | 24) => Some("XAD"),
        (b"ORC", 30) => Some("CNE"),
        // OBR
        (b"OBR", 2 | 3) => Some("EI"),
        (b"OBR", 4 | 12 | 31 | 38 | 39 | 40 | 43 | 44..=48 | 50) => Some("CWE"),
        (b"OBR", 6 | 7 | 8 | 14 | 22 | 36) => Some("TS"),
        (b"OBR", 9) => Some("CQ"),
        (b"OBR", 10 | 16 | 28) => Some("XCN"),
        (b"OBR", 15) => Some("SPS"),
        (b"OBR", 17) => Some("XTN"),
        (b"OBR", 23) => Some("MOC"),
        (b"OBR", 26) => Some("PRL"),
        (b"OBR", 27) => Some("TQ"),
        (b"OBR", 29) => Some("EIP"),
        (b"OBR", 32..=35) => Some("CNN"),
        // OBX
        (b"OBX", 3 | 5 | 6 | 15 | 17) => Some("CE"),
        (b"OBX", 12 | 14 | 19) => Some("TS"),
        (b"OBX", 16) => Some("XCN"),
        (b"OBX", 18) => Some("EI"),
        (b"OBX", 23) => Some("XON"),
        // NTE
        (b"NTE", 4) => Some("CE"),
        // DG1
        (b"DG1", 3 | 7 | 8) => Some("CE"),
        (b"DG1", 5) => Some("TS"),
        (b"DG1", 16) => Some("XCN"),
        // PR1
        (b"PR1", 3) => Some("CE"),
        (b"PR1", 5) => Some("TS"),
        (b"PR1", 8 | 11 | 12) => Some("XCN"),
        // AL1
        (b"AL1", 2..=4) => Some("CE"),
        // GT1
        (b"GT1", 2 | 20 | 30) => Some("CX"),
        (b"GT1", 3 | 4 | 16 | 17 | 43 | 46) => Some("XPN"),
        (b"GT1", 5 | 18) => Some("XAD"),
        (b"GT1", 6 | 7 | 19 | 47) => Some("XTN"),
        (b"GT1", 8 | 25) => Some("TS"),
        (b"GT1", 11 | 24 | 27 | 31 | 36 | 37 | 39 | 42 | 44 | 45 | 48 | 56) => Some("CE"),
        (b"GT1", 22 | 52) => Some("XON"),
        (b"GT1", 28) => Some("CP"),
        (b"GT1", 51) => Some("JCC"),
        (b"GT1", 55) => Some("FC"),
        // ROL
        (b"ROL", 1) => Some("EI"),
        (b"ROL", 3 | 7..=10) => Some("CE"),
        (b"ROL", 4) => Some("XCN"),
        (b"ROL", 5 | 6) => Some("TS"),
        (b"ROL", 11) => Some("XAD"),
        (b"ROL", 12) => Some("XTN"),
        (b"ROL", 13) => Some("PL"),
        // IN1
        (b"IN1", 2 | 17) => Some("CE"),
        (b"IN1", 3 | 10) => Some("CX"),
        (b"IN1", 4 | 11) => Some("XON"),
        (b"IN1", 5 | 19) => Some("XAD"),
        (b"IN1", 6 | 16) => Some("XPN"),
        (b"IN1", 7) => Some("XTN"),
        (b"IN1", 14) => Some("AUI"),
        (b"IN1", 18) => Some("TS"),
        // IN2
        (b"IN2", 1 | 61) => Some("CX"),
        (b"IN2", 3) => Some("XCN"),
        (b"IN2", 63) => Some("XTN"),
        // SCH
        (b"SCH", 1 | 2 | 4 | 23 | 24 | 26 | 27) => Some("EI"),
        (b"SCH", 5..=8 | 10 | 25) => Some("CE"),
        (b"SCH", 11) => Some("TQ"),
        (b"SCH", 12 | 16 | 20) => Some("XCN"),
        (b"SCH", 13 | 17 | 21) => Some("XTN"),
        (b"SCH", 14 | 18) => Some("XAD"),
        (b"SCH", 15 | 19 | 22) => Some("PL"),
        // RGS / AIS / AIG / AIL / AIP
        (b"RGS", 3) => Some("CE"),
        (b"AIS", 3 | 6 | 8 | 10..=12) => Some("CE"),
        (b"AIS", 4) => Some("TS"),
        (b"AIG", 3..=5 | 7 | 10 | 12 | 14) => Some("CE"),
        (b"AIG", 8) => Some("TS"),
        (b"AIL", 3) => Some("PL"),
        (b"AIL", 4 | 5 | 8 | 10 | 12) => Some("CE"),
        (b"AIL", 6) => Some("TS"),
        (b"AIP", 3) => Some("XCN"),
        (b"AIP", 4 | 5 | 8 | 10 | 12) => Some("CE"),
        (b"AIP", 6) => Some("TS"),
        // TQ1 / TQ2
        (b"TQ1", 2 | 5 | 6 | 13) => Some("CQ"),
        (b"TQ1", 3) => Some("RPT"),
        (b"TQ1", 7 | 8) => Some("TS"),
        (b"TQ1", 9) => Some("CWE"),
        (b"TQ2", 3..=5) => Some("EI"),
        // FT1
        (b"FT1", 4 | 5) => Some("TS"),
        (b"FT1", 7 | 13 | 14 | 19 | 25) => Some("CE"),
        (b"FT1", 11 | 12 | 22) => Some("CP"),
        (b"FT1", 16) => Some("PL"),
        (b"FT1", 20 | 21) => Some("XCN"),
        // MRG
        (b"MRG", 1..=6) => Some("CX"),
        (b"MRG", 7) => Some("XPN"),
        // MSA / ERR
        (b"MSA", 6) => Some("CE"),
        (b"ERR", 1) => Some("ELD"),
        (b"ERR", 2) => Some("ERL"),
        (b"ERR", 3 | 5) => Some("CWE"),
        // RXO / RXE / RXR / RXC / RXA
        (b"RXO", 1 | 4..=7 | 10 | 12 | 19 | 20 | 22) => Some("CE"),
        (b"RXO", 8) => Some("LA1"),
        (b"RXO", 14 | 15) => Some("XCN"),
        (b"RXO", 23) => Some("CQ"),
        (b"RXE", 1) => Some("TQ"),
        (b"RXE", 2 | 5..=7 | 11 | 21 | 24 | 26 | 27 | 29 | 31) => Some("CE"),
        (b"RXE", 8) => Some("LA1"),
        (b"RXE", 13 | 14) => Some("XCN"),
        (b"RXE", 18) => Some("TS"),
        (b"RXE", 19) => Some("CQ"),
        (b"RXR", 1..=6) => Some("CWE"),
        (b"RXC", 2 | 4 | 6 | 7) => Some("CE"),
        (b"RXA", 3 | 4) => Some("TS"),
        (b"RXA", 5 | 7 | 8 | 9 | 17) => Some("CE"),
        (b"RXA", 10) => Some("XCN"),
        (b"RXA", 11) => Some("LA2"),
        // QAK / QPD / RCP
        (b"QAK", 3) => Some("CE"),
        (b"QPD", 1) => Some("CE"),
        (b"RCP", 2) => Some("CQ"),
        (b"RCP", 3) => Some("CE"),
        (b"RCP", 4) => Some("TS"),
        (b"RCP", 6) => Some("SRT"),
        // PRD / CTD
        (b"PRD", 1 | 6) => Some("CE"),
        (b"PRD", 2) => Some("XPN"),
        (b"PRD", 3) => Some("XAD"),
        (b"PRD", 4) => Some("PL"),
        (b"PRD", 5) => Some("XTN"),
        (b"PRD", 7) => Some("PLN"),
        (b"PRD", 8 | 9) => Some("TS"),
        (b"CTD", 1 | 6) => Some("CE"),
        (b"CTD", 2) => Some("XPN"),
        (b"CTD", 3) => Some("XAD"),
        (b"CTD", 4) => Some("PL"),
        (b"CTD", 5) => Some("XTN"),
        (b"CTD", 7) => Some("PLN"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // §5.4
    #[test]
    fn builtin_dictionary_covers_all_required_segments_and_supports_z_overlay() {
        let mut dict = Hl7Dictionary::builtin();
        let required = [
            b"MSH", b"EVN", b"PID", b"PD1", b"NK1", b"PV1", b"PV2", b"OBR", b"OBX", b"ORC", b"NTE",
            b"DG1", b"PR1", b"IN1", b"AL1", b"MSA", b"ERR", b"SCH", b"RXA",
        ];
        for tag in required {
            assert!(
                dict.segment_description(tag).is_some(),
                "missing segment description for {}",
                String::from_utf8_lossy(tag)
            );
        }
        let pid3: FieldAddress = "PID-3".parse().unwrap();
        assert_eq!(
            dict.field_description(&pid3),
            Some("Patient Identifier List")
        );

        // User Z-segment overlay (§5.4)
        dict.merge_toml_str(
            r#"
            [segments.ZPD]
            description = "Custom Patient Demographics"
            fields = { "1" = "Custom Flag", "2" = "Department Code" }
            "#,
        )
        .unwrap();
        assert_eq!(
            dict.segment_description(b"ZPD"),
            Some("Custom Patient Demographics")
        );
        let zpd2: FieldAddress = "ZPD-2".parse().unwrap();
        assert_eq!(dict.field_description(&zpd2), Some("Department Code"));
    }

    #[test]
    fn resolves_hl7_data_type_component_and_subcomponent_names_and_tooltips() {
        let dict = Hl7Dictionary::builtin();

        let cases = [
            // Top-level fields still return field name
            ("MSH-9", "Message Type"),
            ("PID-5", "Patient Name"),
            ("OBR-16", "Ordering Provider"),
            // MSG
            ("MSH-9.1", "Message Code"),
            ("MSH-9.2", "Trigger Event"),
            ("MSH-9.3", "Message Structure"),
            // XPN + FN subcomponent
            ("PID-5.1", "Family Name"),
            ("PID-5.1.1", "Surname"),
            ("PID-5.2", "Given Name"),
            // CX + HD subcomponent
            ("PID-3.1", "ID Number"),
            ("PID-3.4", "Assigning Authority"),
            ("PID-3.4.1", "Namespace ID"),
            ("PID-3.4.2", "Universal ID"),
            ("PID-3.5", "Identifier Type Code"),
            // PL
            ("PV1-3.1", "Point of Care"),
            ("PV1-3.2", "Room"),
            ("PV1-3.3", "Bed"),
            // TQ
            ("ORC-7.4", "Start Date/Time"),
            // XCN + HD subcomponent
            ("ORC-12.1", "ID Number"),
            ("ORC-12.2", "Family Name"),
            ("OBR-16.1", "ID Number"),
            ("OBR-16.2", "Family Name"),
            ("OBR-16.3", "Given Name"),
            ("OBR-16.9.1", "Namespace ID"),
            // CE
            ("OBR-4.1", "Identifier"),
            ("OBR-4.2", "Text"),
            // CNN / NDL
            ("OBR-32.1", "ID Number"),
            ("OBR-32.2", "Family Name"),
            ("OBR-32.3", "Given Name"),
            // Fallback for primitive field
            ("PID-8.1", "Administrative Sex"),
        ];

        for (addr_str, expected) in cases {
            let addr: FieldAddress = addr_str.parse().unwrap();
            assert_eq!(
                dict.field_description(&addr),
                Some(expected),
                "unexpected description for {addr_str}"
            );
        }

        // Verify hierarchical tooltip formatting
        let pid5_1: FieldAddress = "PID-5.1".parse().unwrap();
        assert_eq!(
            dict.field_tooltip(&pid5_1).as_deref(),
            Some("PID-5.1 — Patient Name › Family Name (XPN.1)")
        );
        let pid3_4_1: FieldAddress = "PID-3.4.1".parse().unwrap();
        assert_eq!(
            dict.field_tooltip(&pid3_4_1).as_deref(),
            Some("PID-3.4.1 — Patient Identifier List › Assigning Authority › Namespace ID (CX.4.1 / HD.1)")
        );
    }

    #[test]
    fn all_mapped_field_data_types_and_nested_subtypes_resolve_valid_components() {
        let dict = Hl7Dictionary::builtin();
        let mut mapped_fields_checked = 0usize;

        for (seg_name, seg_entry) in &dict.segments {
            assert_eq!(seg_name.len(), 3, "segment tag must be 3 chars: {seg_name}");
            let tag: [u8; 3] = seg_name.as_bytes().try_into().unwrap();
            for field_key in seg_entry.fields.keys() {
                let field_num: u16 = field_key
                    .parse()
                    .unwrap_or_else(|_| panic!("invalid field num {seg_name}-{field_key}"));
                if let Some(dt) = field_data_type(&tag, field_num) {
                    mapped_fields_checked += 1;
                    assert!(
                        data_type_component(dt, 1).is_some(),
                        "broken data type '{dt}' on {seg_name}-{field_num}"
                    );
                    for comp in 1..=20 {
                        if let Some(comp_def) = data_type_component(dt, comp) {
                            assert!(!comp_def.name.is_empty());
                            if let Some(sub_dt) = comp_def.sub_type {
                                assert!(
                                    data_type_component(sub_dt, 1).is_some(),
                                    "broken nested sub_type '{sub_dt}' on {dt}.{comp}"
                                );
                            }
                        }
                    }
                }
            }
        }
        assert!(mapped_fields_checked >= 150);

        // Verify newly added data types & segments directly
        for dt in [
            "RPT", "RI", "CP", "MO", "MOC", "CCD", "SN", "DR", "WVI", "WVS", "ED", "RP", "AD",
            "LA1", "LA2", "PPN", "PLN", "DLD", "FC", "JCC", "CF", "ELD", "ERL", "DLN", "AUI",
            "SRT",
        ] {
            assert!(
                data_type_component(dt, 1).is_some(),
                "missing data type definition for {dt}"
            );
        }

        let extra_cases = [
            ("TQ1-3.1", "Repeat Pattern Code"),
            ("TQ1-3.1.2", "Text"),
            ("PV1-20.1", "Financial Class Code"),
            ("PV1-37.1", "Discharge Location"),
            ("OBR-23.1.1", "Quantity"),
            ("ERR-2.1", "Segment ID"),
            ("ERR-2.3", "Field Position"),
            ("NK1-11.1", "Job Code"),
            ("PRD-7.1", "ID Number"),
            ("PRD-7.2", "Type of ID Number"),
            ("RXO-8.1", "Point of Care"),
            ("RXA-11.9", "Street Address"),
            ("AIS-3.2", "Text"),
            ("QPD-1.1", "Identifier"),
            ("RCP-6.1", "Sort-by Field"),
            // ROL-1..13 & GT1-17..57
            ("ROL-1", "Role Instance ID"),
            ("ROL-4", "Role Person"),
            ("ROL-4.2", "Family Name"),
            ("ROL-11.3", "City"),
            ("ROL-13.1", "Point of Care"),
            ("GT1-17", "Guarantor Employer Name"),
            ("GT1-17.1", "Family Name"),
            ("GT1-18", "Guarantor Employer Address"),
            ("GT1-18.3", "City"),
            ("GT1-28.1", "Price"),
            ("GT1-51.1", "Job Code"),
            ("GT1-55.1", "Financial Class Code"),
            ("GT1-57", "Guarantee Duration"),
        ];
        for (addr_str, expected) in extra_cases {
            let addr: FieldAddress = addr_str.parse().unwrap();
            assert_eq!(
                dict.field_description(&addr),
                Some(expected),
                "unexpected description for {addr_str}"
            );
        }

        // Verify all ROL-1..=13 and GT1-1..=57 fields have descriptions
        for f in 1..=13u16 {
            let addr: FieldAddress = format!("ROL-{f}").parse().unwrap();
            assert!(
                dict.field_description(&addr).is_some(),
                "missing description for ROL-{f}"
            );
        }
        for f in 1..=57u16 {
            let addr: FieldAddress = format!("GT1-{f}").parse().unwrap();
            assert!(
                dict.field_description(&addr).is_some(),
                "missing description for GT1-{f}"
            );
        }
    }
}
