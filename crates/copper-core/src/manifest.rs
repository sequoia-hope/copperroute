//!    `#[serde(skip_serializing_if = "Option::is_none")]` on the `Option` fields is that rule.
use std::path::Path;

use serde::{Serialize, Serializer};

use copper_dsn::format::json::to_gson_string_pretty;
use copper_router::score::BoardStatistics;

use crate::SERVER_VERSION;
use crate::job::{RoutingJob, path_util};
use crate::stats_json::GsonBoardStatistics;

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
pub struct RouterJobResourceUsage {
    #[serde(rename = "cpu_time")]
    pub cpu_time_used: f32,
    #[serde(rename = "max_memory")]
    pub max_memory_used: f32,
    #[serde(rename = "peak_memory")]
    pub peak_memory_used: f32,
    #[serde(rename = "io_read")]
    pub io_read: f32,
    #[serde(rename = "io_written")]
    pub io_write: f32,
}

pub const SCHEMA_VERSION: i32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RoutingResultManifest {
    #[serde(rename = "schema_version")]
    pub schema_version: i32,
    #[serde(rename = "generated_at", skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<String>,
    #[serde(rename = "app_version", skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,
    #[serde(rename = "git_sha", skip_serializing_if = "Option::is_none")]
    pub git_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixture: Option<FixtureInfo>,
    #[serde(rename = "settings_snapshot", skip_serializing_if = "Option::is_none")]
    pub settings_snapshot: Option<copper_settings::RouterSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phases: Option<PhaseMetrics>,
    #[serde(
        rename = "board_statistics",
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_board_statistics"
    )]
    pub board_statistics: Option<BoardStatistics>,
    #[serde(rename = "normalized_score", skip_serializing_if = "Option::is_none")]
    pub normalized_score: Option<f32>,
    #[serde(rename = "resource_usage", skip_serializing_if = "Option::is_none")]
    pub resource_usage: Option<RouterJobResourceUsage>,
    #[serde(rename = "final_state", skip_serializing_if = "Option::is_none")]
    pub final_state: Option<String>,
    #[serde(rename = "exit_code")]
    pub exit_code: i32,
    #[serde(rename = "output_written")]
    pub output_written: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
}

fn serialize_board_statistics<S: Serializer>(
    value: &Option<BoardStatistics>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        Some(stats) => GsonBoardStatistics(stats).serialize(serializer),
        None => serializer.serialize_none(),
    }
}

impl Default for RoutingResultManifest {
    fn default() -> RoutingResultManifest {
        RoutingResultManifest {
            schema_version: SCHEMA_VERSION,
            generated_at: None,
            app_version: None,
            git_sha: None,
            fixture: None,
            settings_snapshot: None,
            phases: Some(PhaseMetrics::default()),
            board_statistics: None,
            normalized_score: None,
            resource_usage: None,
            final_state: None,
            exit_code: 0,
            output_written: false,
            backend: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct FixtureInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PhaseMetrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fanout: Option<PhaseDetail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autorouter: Option<PhaseDetail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub optimizer: Option<PhaseDetail>,
}

impl Default for PhaseMetrics {
    fn default() -> PhaseMetrics {
        PhaseMetrics {
            fanout: Some(PhaseDetail::default()),
            autorouter: Some(PhaseDetail::default()),
            optimizer: Some(PhaseDetail::default()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
pub struct PhaseDetail {
    #[serde(rename = "duration_seconds", skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f32>,
    #[serde(rename = "passes_completed", skip_serializing_if = "Option::is_none")]
    pub passes_completed: Option<i32>,
}

impl RoutingResultManifest {
    // `clippy::field_reassign_with_default` objects to: a struct literal would put `:114`'s
    #[allow(clippy::field_reassign_with_default)]
    pub fn from_job(
        job: &RoutingJob,
        input_file_path: Option<&Path>,
        output_written: bool,
        exit_code: i32,
        now: &dyn Fn() -> String,
        stats: Option<&BoardStatistics>,
    ) -> RoutingResultManifest {
        let mut manifest = RoutingResultManifest::default();
        manifest.generated_at = Some(now());
        manifest.app_version = Some(SERVER_VERSION.to_string());
        manifest.git_sha = Some(resolve_git_sha());
        let mut fixture = FixtureInfo::default();
        if let Some(path) = input_file_path {
            let normalized = path_util::of_to_string(&path.to_string_lossy());
            manifest.fixture = {
                fixture.filename =
                    Some(path_util::file_name_of_normalized(&normalized).unwrap_or_default());
                fixture.sha256 = sha256_hex(Path::new(&normalized));
                Some(fixture)
            };
        } else {
            manifest.fixture = Some(fixture);
        }
        manifest.settings_snapshot = Some(job.router_settings.clone());
        manifest.final_state = Some(job.state.name().to_string());
        manifest.exit_code = exit_code;
        manifest.output_written = output_written;
        manifest.resource_usage = Some(job.resource_usage);

        if let Some(stats) = stats {
            manifest.board_statistics = Some(stats.clone());
            if let Some(scoring) = job.router_settings.scoring.as_ref() {
                manifest.normalized_score = Some(stats.normalized_score(scoring));
            }
        }

        let phases = manifest
            .phases
            .as_mut()
            .expect("RoutingResultManifest::default allocates phases");
        let autorouter = phases
            .autorouter
            .as_mut()
            .expect("PhaseMetrics::default allocates autorouter");
        if job.get_current_pass() > 0 {
            autorouter.passes_completed = Some(job.get_current_pass());
        }

        if let (Some(started), Some(finished)) = (job.started_at, job.finished_at) {
            let millis = finished.saturating_duration_since(started).as_millis();
            autorouter.duration_seconds = Some((millis as f64 / 1000.0) as f32);
        }

        manifest
    }

    pub fn write(path: &Path, manifest: &RoutingResultManifest) -> std::io::Result<()> {
        let normalized = path_util::of_to_string(&path.to_string_lossy());
        if let Some(parent) = path_util::parent_of_normalized(&normalized) {
            std::fs::create_dir_all(&parent)?;
        }
        let json = to_gson_string_pretty(manifest)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&normalized, json.as_bytes())
    }

    pub fn to_gson_string(&self) -> Result<String, serde_json::Error> {
        to_gson_string_pretty(self)
    }
}

pub fn resolve_git_sha() -> String {
    if let Ok(value) = std::env::var("COPPERROUTE_GIT_SHA")
        && !(&value).trim().is_empty()
    {
        return value.trim().to_string();
    }
    "unknown".to_string()
}

pub fn sha256_hex(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(hex_lower(&sha256(&bytes)))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit(u32::from(byte >> 4), 16).expect("a nibble is < 16"));
        out.push(char::from_digit(u32::from(byte & 0x0F), 16).expect("a nibble is < 16"));
    }
    out
}

const SHA256_H0: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

#[rustfmt::skip]
const SHA256_K: [u32; 64] = [
    0x428a_2f98, 0x7137_4491, 0xb5c0_fbcf, 0xe9b5_dba5, 0x3956_c25b, 0x59f1_11f1, 0x923f_82a4, 0xab1c_5ed5,
    0xd807_aa98, 0x1283_5b01, 0x2431_85be, 0x550c_7dc3, 0x72be_5d74, 0x80de_b1fe, 0x9bdc_06a7, 0xc19b_f174,
    0xe49b_69c1, 0xefbe_4786, 0x0fc1_9dc6, 0x240c_a1cc, 0x2de9_2c6f, 0x4a74_84aa, 0x5cb0_a9dc, 0x76f9_88da,
    0x983e_5152, 0xa831_c66d, 0xb003_27c8, 0xbf59_7fc7, 0xc6e0_0bf3, 0xd5a7_9147, 0x06ca_6351, 0x1429_2967,
    0x27b7_0a85, 0x2e1b_2138, 0x4d2c_6dfc, 0x5338_0d13, 0x650a_7354, 0x766a_0abb, 0x81c2_c92e, 0x9272_2c85,
    0xa2bf_e8a1, 0xa81a_664b, 0xc24b_8b70, 0xc76c_51a3, 0xd192_e819, 0xd699_0624, 0xf40e_3585, 0x106a_a070,
    0x19a4_c116, 0x1e37_6c08, 0x2748_774c, 0x34b0_bcb5, 0x391c_0cb3, 0x4ed8_aa4a, 0x5b9c_ca4f, 0x682e_6ff3,
    0x748f_82ee, 0x78a5_636f, 0x84c8_7814, 0x8cc7_0208, 0x90be_fffa, 0xa450_6ceb, 0xbef9_a3f7, 0xc671_78f2,
];

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = SHA256_H0;

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity(data.len() + 72);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for block in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (index, word) in block.chunks_exact(4).enumerate() {
            w[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for index in 16..64 {
            let s0 = w[index - 15].rotate_right(7)
                ^ w[index - 15].rotate_right(18)
                ^ (w[index - 15] >> 3);
            let s1 = w[index - 2].rotate_right(17)
                ^ w[index - 2].rotate_right(19)
                ^ (w[index - 2] >> 10);
            w[index] = w[index - 16]
                .wrapping_add(s0)
                .wrapping_add(w[index - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for index in 0..64 {
            let big_s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(big_s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[index])
                .wrapping_add(w[index]);
            let big_s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = big_s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }

        for (slot, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(value);
        }
    }

    let mut digest = [0u8; 32];
    for (chunk, word) in digest.chunks_exact_mut(4).zip(h) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    digest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex_of(bytes: &[u8]) -> String {
        hex_lower(&sha256(bytes))
    }

    #[test]
    fn sha256_matches_the_nist_vectors() {
        assert_eq!(
            hex_of(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex_of(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            hex_of(&vec![b'a'; 1_000_000]),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
        assert_eq!(
            hex_of(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sha256_pads_across_the_block_boundary() {
        assert_eq!(
            hex_of(&[b'a'; 55]),
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"
        );
        assert_eq!(
            hex_of(&[b'a'; 56]),
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
        );
        assert_eq!(
            hex_of(&[b'a'; 64]),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
    }

    #[test]
    fn hex_is_lower_case_and_zero_padded() {
        assert_eq!(hex_lower(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
    }
}

#[must_use]
pub fn now_utc_iso8601() -> String {
    format_utc_iso8601(std::time::SystemTime::now())
}

#[must_use]
pub fn format_utc_iso8601(time: std::time::SystemTime) -> String {
    let (secs, nanos) = match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(delta) => (
            i64::try_from(delta.as_secs()).unwrap_or(i64::MAX),
            delta.subsec_nanos(),
        ),
        Err(error) => {
            let delta = error.duration();
            let secs = i64::try_from(delta.as_secs()).unwrap_or(i64::MAX);
            match delta.subsec_nanos() {
                0 => (-secs, 0),
                sub => (-secs - 1, 1_000_000_000 - sub),
            }
        }
    };
    let days = secs.div_euclid(86_400);
    let seconds_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60,
    );
    let mut out = format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}");
    if nanos != 0 {
        if nanos % 1_000_000 == 0 {
            out.push_str(&format!(".{:03}", nanos / 1_000_000));
        } else if nanos % 1_000 == 0 {
            out.push_str(&format!(".{:06}", nanos / 1_000));
        } else {
            out.push_str(&format!(".{nanos:09}"));
        }
    }
    out.push('Z');
    out
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    (year, m as u32, d as u32)
}

#[cfg(test)]
mod instant_tests {
    use super::*;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn the_rendering_is_the_jvms_instant_to_string() {
        let base = 1_756_800_000u64;
        for (nanos, expected) in [
            (0u32, "2025-09-02T08:00:00Z"),
            (1, "2025-09-02T08:00:00.000000001Z"),
            (1_000, "2025-09-02T08:00:00.000001Z"),
            (1_000_000, "2025-09-02T08:00:00.001Z"),
            (10_000_000, "2025-09-02T08:00:00.010Z"),
            (100_000_000, "2025-09-02T08:00:00.100Z"),
            (120_000_000, "2025-09-02T08:00:00.120Z"),
            (123_000_000, "2025-09-02T08:00:00.123Z"),
            (123_456_789, "2025-09-02T08:00:00.123456789Z"),
            (500_000_000, "2025-09-02T08:00:00.500Z"),
            (999_999_999, "2025-09-02T08:00:00.999999999Z"),
        ] {
            let time = UNIX_EPOCH + Duration::new(base, nanos);
            assert_eq!(format_utc_iso8601(time), expected, "nanos {nanos}");
        }
        assert_eq!(format_utc_iso8601(UNIX_EPOCH), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn the_calendar_is_proleptic_gregorian() {
        for (secs, expected) in [
            (951_782_400u64, "2000-02-29T00:00:00Z"),
            (951_868_800, "2000-03-01T00:00:00Z"),
            (1_072_915_199, "2003-12-31T23:59:59Z"),
            (1_072_915_200, "2004-01-01T00:00:00Z"),
            (4_102_444_800, "2100-01-01T00:00:00Z"),
        ] {
            assert_eq!(
                format_utc_iso8601(UNIX_EPOCH + Duration::from_secs(secs)),
                expected,
                "secs {secs}"
            );
        }
    }

    #[test]
    fn now_is_well_formed() {
        let now = now_utc_iso8601();
        assert!(now.ends_with('Z'), "{now}");
        assert_eq!(now.as_bytes()[4], b'-', "{now}");
        assert_eq!(now.as_bytes()[10], b'T', "{now}");
        assert!(now.starts_with("20"), "{now}");
        assert!(now_utc_iso8601() >= now);
        let _ = SystemTime::now();
    }
}
