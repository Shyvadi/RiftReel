//! Which encoder records on this machine, found once and stored (doc 03 §5): NVENC →
//! AMF → QSV, then the CPU with a warning. A candidate must be in the ffmpeg build and
//! encode a few test frames here; the result is kept in a small JSON file and reused
//! until a different ffmpeg binary turns up.

use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::ffmpeg::{self, EncoderFamily, Priority};

/// Probe order. x264 is GPL and not in the LGPL build Downplayed ships (it is used if a
/// configured ffmpeg has it); there the CPU fallback is OpenH264.
pub const CANDIDATES: [&str; 5] = [
    "h264_nvenc",
    "h264_amf",
    "h264_qsv",
    "libx264",
    "libopenh264",
];

const PROBE_SCHEMA: u32 = 1;
const TEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Longest rejection reason kept (ffmpeg errors can be chatty).
const MAX_REASON: usize = 300;

/// Identifies the probed binary; another file at the path (an update) is probed again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FfmpegFingerprint {
    pub path: String,
    pub size: u64,
    pub modified_ms: u64,
}

impl FfmpegFingerprint {
    pub fn of(path: &Path) -> io::Result<Self> {
        let meta = std::fs::metadata(path)?;
        let modified_ms = meta
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or_default();
        Ok(Self {
            path: path.display().to_string(),
            size: meta.len(),
            modified_ms,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejected {
    pub encoder: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    pub schema: u32,
    pub ffmpeg: FfmpegFingerprint,
    pub version: Option<String>,
    /// The build has the Desktop Duplication source capture needs.
    pub ddagrab: bool,
    /// What to record with; `None` when nothing works.
    pub encoder: Option<String>,
    /// Candidates passed over, in probe order, and why.
    pub rejected: Vec<Rejected>,
    pub probed_at_ms: u64,
}

impl ProbeResult {
    /// Still valid for this binary.
    pub fn is_for(&self, ffmpeg: &FfmpegFingerprint) -> bool {
        self.schema == PROBE_SCHEMA && self.ffmpeg == *ffmpeg
    }

    /// The probe settled on a CPU encoder: recording works but costs game performance.
    pub fn is_software(&self) -> bool {
        self.encoder
            .as_deref()
            .is_some_and(|e| !EncoderFamily::of(e).is_hardware())
    }
}

/// `ffmpeg version n8.1.3-9-g29e619e767-20260930 Copyright …` → `n8.1.3-9-g29e619e767-20260930`.
pub fn parse_version(text: &str) -> Option<String> {
    let line = text
        .lines()
        .find(|l| l.trim_start().starts_with("ffmpeg version "))?;
    line.trim_start()
        .strip_prefix("ffmpeg version ")?
        .split_whitespace()
        .next()
        .map(str::to_owned)
}

/// Video encoder names from `ffmpeg -encoders`.
pub fn parse_encoders(text: &str) -> Vec<String> {
    text.lines()
        .skip_while(|l| !l.trim_start().starts_with("---"))
        .skip(1)
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let flags = fields.next()?;
            let name = fields.next()?;
            (flags.len() == 6 && flags.starts_with('V')).then(|| name.to_owned())
        })
        .collect()
}

/// Filter names from `ffmpeg -filters` (rows look like ` .. ddagrab  |->V  Grab …`).
pub fn parse_filters(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let _flags = fields.next()?;
            let name = fields.next()?;
            fields.next()?.contains("->").then(|| name.to_owned())
        })
        .collect()
}

/// The candidates this build has, in probe order, and the ones it lacks.
pub fn candidates(available: &[String]) -> (Vec<&'static str>, Vec<Rejected>) {
    let mut test = Vec::new();
    let mut missing = Vec::new();
    for candidate in CANDIDATES {
        if available.iter().any(|name| name == candidate) {
            test.push(candidate);
        } else {
            missing.push(Rejected {
                encoder: candidate.into(),
                reason: "not in this ffmpeg build".into(),
            });
        }
    }
    (test, missing)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

async fn ffmpeg_text(ffmpeg: &Path, args: &[&str]) -> io::Result<String> {
    let args: Vec<_> = args.iter().map(Into::into).collect();
    let (exit, stdout) = ffmpeg::run_to_end(ffmpeg, &args, Priority::Normal, TEST_TIMEOUT).await?;
    match exit.detail {
        Some(detail) if !exit.success => Err(io::Error::other(detail)),
        _ => Ok(stdout),
    }
}

/// Encodes ten frames of a test pattern with `encoder` on this machine.
pub async fn test_encode(ffmpeg: &Path, encoder: &str) -> Result<(), String> {
    let mut args = vec![
        "-hide_banner",
        "-nostdin",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=1280x720:r=60",
        "-frames:v",
        "10",
        "-c:v",
        encoder,
    ];
    if !EncoderFamily::of(encoder).is_hardware() {
        args.extend(["-pix_fmt", "yuv420p"]);
    }
    args.extend(["-f", "null", "-"]);
    let args: Vec<_> = args.iter().map(Into::into).collect();
    match ffmpeg::run_to_end(ffmpeg, &args, Priority::Normal, TEST_TIMEOUT).await {
        Ok((exit, _)) if exit.success => Ok(()),
        Ok((exit, _)) => Err(exit.detail.unwrap_or_else(|| "failed".into())),
        Err(e) => Err(e.to_string()),
    }
}

fn truncate(mut reason: String) -> String {
    if reason.len() > MAX_REASON {
        let mut cut = MAX_REASON;
        while !reason.is_char_boundary(cut) {
            cut -= 1;
        }
        reason.truncate(cut);
        reason.push('…');
    }
    reason
}

/// Probes `ffmpeg`: version, `ddagrab`, then the candidates in order until one encodes.
pub async fn probe(ffmpeg: &Path) -> io::Result<ProbeResult> {
    let fingerprint = FfmpegFingerprint::of(ffmpeg)?;
    let version = parse_version(&ffmpeg_text(ffmpeg, &["-hide_banner", "-version"]).await?);
    let ddagrab = parse_filters(&ffmpeg_text(ffmpeg, &["-hide_banner", "-filters"]).await?)
        .iter()
        .any(|f| f == "ddagrab");
    let available = parse_encoders(&ffmpeg_text(ffmpeg, &["-hide_banner", "-encoders"]).await?);
    let (to_test, mut rejected) = candidates(&available);
    let mut encoder = None;
    if ddagrab {
        for candidate in to_test {
            match test_encode(ffmpeg, candidate).await {
                Ok(()) => {
                    encoder = Some(candidate.to_owned());
                    break;
                }
                Err(reason) => rejected.push(Rejected {
                    encoder: candidate.into(),
                    reason: truncate(reason),
                }),
            }
        }
    }
    // Probe order, whichever way each candidate was turned down.
    rejected.sort_by_key(|r| CANDIDATES.iter().position(|c| *c == r.encoder));
    Ok(ProbeResult {
        schema: PROBE_SCHEMA,
        ffmpeg: fingerprint,
        version,
        ddagrab,
        encoder,
        rejected,
        probed_at_ms: now_ms(),
    })
}

pub fn load(path: &Path) -> Option<ProbeResult> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn store(path: &Path, result: &ProbeResult) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(result).map_err(io::Error::other)?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, text + "\n")?;
    std::fs::rename(&temp, path)
}

/// The stored result for this ffmpeg, or a new probe (then stored). `force` probes
/// again regardless.
pub async fn cached_or_probe(ffmpeg: &Path, cache: &Path, force: bool) -> io::Result<ProbeResult> {
    let fingerprint = FfmpegFingerprint::of(ffmpeg)?;
    if !force {
        if let Some(cached) = load(cache).filter(|c| c.is_for(&fingerprint)) {
            return Ok(cached);
        }
    }
    let result = probe(ffmpeg).await?;
    match &result.encoder {
        Some(encoder) if result.is_software() => tracing::warn!(
            %encoder,
            "no hardware encoder works here; recording on the CPU will cost game performance"
        ),
        Some(encoder) => tracing::info!(%encoder, version = ?result.version, "encoder probe"),
        None if !result.ddagrab => {
            tracing::warn!(ffmpeg = %ffmpeg.display(), "this ffmpeg has no ddagrab; capture is off")
        }
        None => tracing::warn!(rejected = ?result.rejected, "no encoder works; capture is off"),
    }
    if let Err(e) = store(cache, &result) {
        tracing::warn!(error = %e, "could not store the encoder probe");
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENCODERS: &str = "Encoders:
 V..... = Video
 A..... = Audio
 S..... = Subtitle
 .F.... = Frame-level multithreading
 ..S... = Slice-level multithreading
 ...X.. = Codec is experimental
 ....B. = Supports draw_horiz_band
 .....D = Supports direct rendering method 1
 ------
 V....D libopenh264          OpenH264 H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10 (codec h264)
 V....D h264_amf             AMD AMF H.264 Encoder (codec h264)
 V....D h264_nvenc           NVIDIA NVENC H.264 encoder (codec h264)
 V..... h264_qsv             H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10 (Intel Quick Sync Video acceleration) (codec h264)
 V....D hevc_nvenc           NVIDIA NVENC hevc encoder (codec hevc)
 A....D aac                  AAC (Advanced Audio Coding)
 S..... ass                  ASS (Advanced SubStation Alpha) subtitle
";

    const FILTERS: &str = "Filters:
  T.. = Timeline support
  .S. = Slice threading
  A = Audio input/output
  V = Video input/output
  N = Dynamic number and/or type of input/output
  | = Source or sink filter
 .. abench            A->A       Benchmark part of a filtergraph.
 .. ddagrab           |->V       Grab Windows Desktop images using Desktop Duplication API
 T. setpts            V->V       Set PTS for the output video frame.
";

    #[test]
    fn version_line_parses() {
        let text = "ffmpeg version n8.1.3-9-g29e619e767-20260930 Copyright (c) 2000-2026 the FFmpeg developers\nbuilt with gcc 15.2.0";
        assert_eq!(
            parse_version(text).as_deref(),
            Some("n8.1.3-9-g29e619e767-20260930")
        );
        assert_eq!(
            parse_version("ffmpeg version 5.1.2-essentials_build-www.gyan.dev Copyright")
                .as_deref(),
            Some("5.1.2-essentials_build-www.gyan.dev")
        );
        assert_eq!(parse_version("nothing here"), None);
    }

    #[test]
    fn encoder_list_parses_video_encoders_only() {
        assert_eq!(
            parse_encoders(ENCODERS),
            [
                "libopenh264",
                "h264_amf",
                "h264_nvenc",
                "h264_qsv",
                "hevc_nvenc"
            ]
        );
        assert!(parse_encoders("").is_empty());
    }

    #[test]
    fn filter_list_parses() {
        let filters = parse_filters(FILTERS);
        assert_eq!(filters, ["abench", "ddagrab", "setpts"]);
        // ffmpeg 5.1 "essentials": no Desktop Duplication.
        assert!(!parse_filters(" .. gdigrab ... no").contains(&"ddagrab".to_owned()));
    }

    #[test]
    fn candidates_keep_the_probe_order_and_skip_what_the_build_lacks() {
        let (test, missing) = candidates(&parse_encoders(ENCODERS));
        assert_eq!(test, ["h264_nvenc", "h264_amf", "h264_qsv", "libopenh264"]);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].encoder, "libx264");
        assert_eq!(missing[0].reason, "not in this ffmpeg build");

        let gpl_build = vec!["libx264".to_owned(), "h264_nvenc".to_owned()];
        assert_eq!(candidates(&gpl_build).0, ["h264_nvenc", "libx264"]);
    }

    #[test]
    fn stored_results_are_reused_only_for_the_same_binary() {
        let dir = std::env::temp_dir().join(format!("downplayed-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cache = dir.join("capture-probe.json");
        let fingerprint = FfmpegFingerprint {
            path: r"C:\Downplayed\ffmpeg.exe".into(),
            size: 134_088_704,
            modified_ms: 1_792_080_720_000,
        };
        let result = ProbeResult {
            schema: PROBE_SCHEMA,
            ffmpeg: fingerprint.clone(),
            version: Some("n8.1.3".into()),
            ddagrab: true,
            encoder: Some("h264_nvenc".into()),
            rejected: vec![],
            probed_at_ms: 1,
        };
        store(&cache, &result).unwrap();
        let loaded = load(&cache).unwrap();
        assert_eq!(loaded, result);
        assert!(loaded.is_for(&fingerprint));
        assert!(!loaded.is_software());
        let updated = FfmpegFingerprint {
            size: fingerprint.size + 1,
            ..fingerprint.clone()
        };
        assert!(!loaded.is_for(&updated), "a new ffmpeg is probed again");
        assert!(!ProbeResult {
            schema: 0,
            ..result.clone()
        }
        .is_for(&fingerprint));
        assert!(ProbeResult {
            encoder: Some("libopenh264".into()),
            ..result
        }
        .is_software());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn long_reasons_are_cut() {
        let reason = truncate("é".repeat(400));
        assert!(reason.len() <= MAX_REASON + '…'.len_utf8());
        assert!(reason.ends_with('…'));
    }
}
