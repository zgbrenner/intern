use std::path::PathBuf;

#[cfg(feature = "native-tesseract")]
use std::io::{Cursor, Read};
#[cfg(feature = "native-tesseract")]
use std::path::Path;
#[cfg(feature = "native-tesseract")]
use std::process::{Command, Stdio};
#[cfg(feature = "native-tesseract")]
use std::time::Duration;

#[cfg(feature = "native-tesseract")]
use image::{DynamicImage, ImageFormat};

#[cfg(feature = "native-tesseract")]
use crate::extract::{CONFIDENT_READING, apply_detected_rotation};
use crate::extract::{CancellationToken, ExtractionError, OcrBackend, OcrResult, RenderedPage};
#[cfg(feature = "native-tesseract")]
use crate::limits::ResourceLimits;
#[cfg(feature = "native-tesseract")]
use crate::temp::TempWorkspace;

/// How to ask Tesseract for TSV output without depending on a file we do not ship.
///
/// `tesseract in out tsv` does not pass a flag: `tsv` names a config file that
/// must exist at `tessdata/configs/tsv`. The pinned tessdata is two
/// `.traineddata` files and nothing else, so on a packaged install Tesseract
/// warned `read_params_file: Can't open tsv`, fell back to its default renderer,
/// exited 0, and wrote a `.txt`. Reading the `.tsv` then failed with "The system
/// cannot find the file specified", which surfaced as a parse failure on every
/// scanned document. A development machine with a full Tesseract install has that
/// config file, which is exactly why this passed locally and failed on a real
/// package.
#[cfg(feature = "native-tesseract")]
const TSV_RENDERER: [&str; 2] = ["-c", "tessedit_create_tsv=1"];

#[cfg(feature = "native-tesseract")]
#[derive(Clone, Debug)]
pub struct TesseractOcr {
    executable: PathBuf,
    tessdata_directory: PathBuf,
    language: String,
}

#[cfg(feature = "native-tesseract")]
impl TesseractOcr {
    fn osd_output_path(base: &Path) -> PathBuf {
        base.with_extension("osd")
    }

    pub fn new(
        executable: impl Into<PathBuf>,
        tessdata_directory: impl Into<PathBuf>,
    ) -> Result<Self, ExtractionError> {
        let executable = executable.into();
        let tessdata_directory = tessdata_directory.into();
        Ok(Self {
            executable,
            tessdata_directory,
            language: "eng".to_owned(),
        })
    }

    fn parse_tsv(&self, bytes: &[u8]) -> Result<OcrResult, ExtractionError> {
        let text = String::from_utf8(bytes.to_vec())
            .map_err(|error| ExtractionError::parse_failed(error.to_string()))?;
        let mut words = Vec::new();
        let mut confidences = Vec::new();
        for line in text.lines().skip(1) {
            let columns: Vec<&str> = line.splitn(12, '\t').collect();
            if columns.len() != 12 {
                continue;
            }
            let word = columns[11].trim();
            let confidence = columns[10].parse::<f32>().unwrap_or(-1.0);
            if !word.is_empty() {
                words.push(word);
                if confidence >= 0.0 {
                    confidences.push(confidence);
                }
            }
        }
        let mean_confidence = if confidences.is_empty() {
            0.0
        } else {
            confidences.iter().sum::<f32>() / confidences.len() as f32
        };
        Ok(OcrResult::new(words.join(" "), mean_confidence))
    }

    fn parse_osd(&self, bytes: &[u8]) -> Result<u16, ExtractionError> {
        let text = std::str::from_utf8(bytes)
            .map_err(|error| ExtractionError::parse_failed(error.to_string()))?;
        let rotation = text
            .lines()
            .find_map(|line| line.strip_prefix("Rotate:"))
            .and_then(|value| value.trim().parse::<u16>().ok())
            .ok_or_else(|| {
                ExtractionError::parse_failed("Tesseract OSD did not report a rotation")
            })?;
        match rotation % 360 {
            0 | 90 | 180 | 270 => Ok(rotation % 360),
            _ => Err(ExtractionError::parse_failed(format!(
                "Tesseract OSD reported unsupported rotation {rotation}"
            ))),
        }
    }

    fn is_sparse_osd_diagnostic(diagnostic: &str) -> bool {
        let diagnostic = diagnostic.to_ascii_lowercase();
        let sparse =
            diagnostic.contains("too few characters") || diagnostic.contains("skipping this page");
        sparse && !Self::is_osd_initialization_diagnostic(&diagnostic)
    }

    fn is_osd_initialization_diagnostic(diagnostic: &str) -> bool {
        let diagnostic = diagnostic.to_ascii_lowercase();
        [
            "error opening data file",
            "failed loading language",
            "couldn't load any languages",
            "could not initialize",
            "initialization failed",
            "failed to load",
        ]
        .iter()
        .any(|message| diagnostic.contains(message))
    }

    fn diagnostic_summary(diagnostic: &str) -> String {
        diagnostic
            .chars()
            .take(512)
            .collect::<String>()
            .trim()
            .to_owned()
    }

    fn write_png(
        &self,
        workspace: &TempWorkspace,
        name: &str,
        image: &DynamicImage,
    ) -> Result<PathBuf, ExtractionError> {
        let mut png = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
            .map_err(|error| ExtractionError::parse_failed(error.to_string()))?;
        workspace.write(name, &png)
    }

    fn wait_for_child(
        &self,
        mut child: std::process::Child,
        cancel: &CancellationToken,
    ) -> Result<std::process::ExitStatus, ExtractionError> {
        loop {
            if let Err(error) = cancel.check() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            if let Some(status) = child.try_wait().map_err(ExtractionError::io)? {
                return Ok(status);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn require_success(status: std::process::ExitStatus) -> Result<(), ExtractionError> {
        if status.success() {
            Ok(())
        } else {
            Err(ExtractionError::parse_failed(format!(
                "Tesseract exited with {status}"
            )))
        }
    }

    fn recognize_at(
        &self,
        workspace: &TempWorkspace,
        page: &RenderedPage,
        rotation: u16,
        label: &str,
        cancel: &CancellationToken,
    ) -> Result<OcrResult, ExtractionError> {
        let rotated = apply_detected_rotation(page.image.clone(), rotation)?;
        let input = self.write_png(workspace, &format!("{label}.png"), &rotated)?;
        let output_base = workspace.path().join(format!("ocr-{label}"));
        let child = Command::new(&self.executable)
            .arg(&input)
            .arg(&output_base)
            .arg("-l")
            .arg(&self.language)
            .arg("--tessdata-dir")
            .arg(&self.tessdata_directory)
            .arg("--psm")
            .arg("1")
            .args(TSV_RENDERER)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(ExtractionError::io)?;
        Self::require_success(self.wait_for_child(child, cancel)?)?;
        let output_path = output_base.with_extension("tsv");
        workspace.register_existing(&output_path)?;
        let output = std::fs::read(output_path).map_err(ExtractionError::io)?;
        Ok(self.parse_tsv(&output)?.with_rotation(rotation))
    }

    /// Of two readings of the same page, the one Tesseract is more confident in.
    /// A tie keeps the incumbent, so the orientation OSD chose wins by default
    /// and behaviour on a blank page stays predictable.
    ///
    /// Orientation detection is trained on prose with ascenders and descenders.
    /// On a dense all-caps form it can report a rotation that is 180 degrees
    /// wrong, and OCR then returns a full page of gibberish rather than
    /// obviously empty output - same word count, plausible shape, useless text.
    /// Volume cannot tell those apart; word confidence can. Measured on one
    /// corpus page, the four orientations scored 23, 14, 14, and 76.
    fn better_reading(incumbent: OcrResult, challenger: OcrResult) -> OcrResult {
        if challenger.mean_confidence > incumbent.mean_confidence {
            challenger
        } else {
            incumbent
        }
    }
}

#[cfg(feature = "native-tesseract")]
impl OcrBackend for TesseractOcr {
    fn recognize(
        &self,
        page: &RenderedPage,
        cancel: &CancellationToken,
    ) -> Result<OcrResult, ExtractionError> {
        cancel.check()?;
        if !self.executable.is_file()
            || !self.tessdata_directory.join("eng.traineddata").is_file()
            || !self.tessdata_directory.join("osd.traineddata").is_file()
        {
            return Err(ExtractionError::native_assets_missing(
                "Tesseract executable, eng.traineddata, or osd.traineddata is absent",
            ));
        }
        let workspace =
            TempWorkspace::create("tesseract", ResourceLimits::default().max_temp_bytes)?;
        let input = self.write_png(&workspace, "input.png", &page.image)?;
        let osd_base = workspace.path().join("orientation");
        let osd_stderr_path = workspace.write("osd.stderr", b"")?;
        let osd_stderr = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&osd_stderr_path)
            .map_err(ExtractionError::io)?;
        let osd_child = Command::new(&self.executable)
            .arg(&input)
            .arg(&osd_base)
            .arg("-l")
            .arg("osd")
            .arg("--tessdata-dir")
            .arg(&self.tessdata_directory)
            .arg("--psm")
            .arg("0")
            .stdout(Stdio::null())
            .stderr(Stdio::from(osd_stderr))
            .spawn()
            .map_err(ExtractionError::io)?;
        let osd_status = self.wait_for_child(osd_child, cancel)?;
        workspace.register_existing(&osd_stderr_path)?;
        let mut osd_diagnostic = Vec::new();
        std::fs::File::open(&osd_stderr_path)
            .map_err(ExtractionError::io)?
            .take(64 * 1024)
            .read_to_end(&mut osd_diagnostic)
            .map_err(ExtractionError::io)?;
        let osd_diagnostic = String::from_utf8_lossy(&osd_diagnostic);
        let rotation = if osd_status.success() {
            let osd_path = Self::osd_output_path(&osd_base);
            workspace.register_existing(&osd_path)?;
            self.parse_osd(&std::fs::read(osd_path).map_err(ExtractionError::io)?)?
        } else if osd_status.code() == Some(1) && Self::is_sparse_osd_diagnostic(&osd_diagnostic) {
            // Tesseract uses exit code 1 when OSD cannot determine an
            // orientation for sparse or blank input. OCR remains useful.
            0
        } else if Self::is_osd_initialization_diagnostic(&osd_diagnostic) {
            return Err(ExtractionError::native_assets_missing(format!(
                "Tesseract OSD initialization failed: {}",
                Self::diagnostic_summary(&osd_diagnostic)
            )));
        } else {
            return Err(ExtractionError::parse_failed(format!(
                "Tesseract OSD exited with {osd_status}: {}",
                Self::diagnostic_summary(&osd_diagnostic)
            )));
        };

        let mut best = self.recognize_at(&workspace, page, rotation, "oriented", cancel)?;
        // A page that reads confidently in the orientation OSD asked for is done:
        // the overwhelmingly common upright document still costs exactly one pass.
        for candidate in [270, 90, 180, 0] {
            if best.mean_confidence >= CONFIDENT_READING {
                break;
            }
            if candidate == rotation {
                continue;
            }
            let attempt = self.recognize_at(
                &workspace,
                page,
                candidate,
                &format!("try-{candidate}"),
                cancel,
            )?;
            best = Self::better_reading(best, attempt);
        }
        Ok(best)
    }
}

#[cfg(all(test, feature = "native-tesseract"))]
mod tests {
    use super::{TSV_RENDERER, TesseractOcr};
    use crate::extract::OcrResult;
    use std::path::{Path, PathBuf};

    #[test]
    fn osd_output_contract_uses_osd_extension() {
        assert_eq!(
            TesseractOcr::osd_output_path(Path::new("orientation")),
            PathBuf::from("orientation.osd")
        );
    }

    #[test]
    fn osd_exit_one_requires_sparse_diagnostic_without_initialization_failure() {
        assert!(TesseractOcr::is_sparse_osd_diagnostic(
            "Too few characters. Skipping this page"
        ));
        assert!(!TesseractOcr::is_sparse_osd_diagnostic(""));
        assert!(!TesseractOcr::is_sparse_osd_diagnostic(
            "Skipping this page; Error opening data file osd.traineddata; \
             Failed loading language osd; Could not initialize tesseract"
        ));
    }

    /// The packaged tessdata has no `configs/` directory, so TSV output has to be
    /// requested as a parameter. Naming the `tsv` config instead made Tesseract
    /// write a `.txt` and succeed, and every scanned document then failed to
    /// parse.
    #[test]
    fn tsv_output_is_requested_by_parameter_not_by_a_config_file() {
        assert_eq!(TSV_RENDERER, ["-c", "tessedit_create_tsv=1"]);
        assert!(
            !TSV_RENDERER.contains(&"tsv"),
            "a bare `tsv` argument names tessdata/configs/tsv, which is not shipped"
        );
    }

    /// Measured on the corpus: a page read in the orientation OSD asked for
    /// scored 44 while the same page read as-is scored 95, with both readings
    /// returning eleven words. Volume cannot choose between them; confidence
    /// can.
    #[test]
    fn a_confidently_misdetected_rotation_loses_to_the_page_as_it_was() {
        let oriented = OcrResult::new("O71 TIVL3Y MOGVAW ZLYVNO", 44.1).with_rotation(180);
        let unrotated = OcrResult::new("PACKING SLIP PS-311 DATE JULY 15 2025", 95.2);
        let chosen = TesseractOcr::better_reading(oriented, unrotated);
        assert_eq!(chosen.text, "PACKING SLIP PS-311 DATE JULY 15 2025");
        assert_eq!(chosen.rotation_degrees, 0);
    }

    #[test]
    fn a_genuinely_rotated_page_keeps_the_rotation_that_read_it() {
        let oriented = OcrResult::new("DELIVERY RECEIPT DR-771", 92.0).with_rotation(270);
        let unrotated = OcrResult::new("gibberish", 31.0);
        let chosen = TesseractOcr::better_reading(oriented, unrotated);
        assert_eq!(chosen.text, "DELIVERY RECEIPT DR-771");
        assert_eq!(chosen.rotation_degrees, 270);
    }

    /// A tie keeps the detected orientation rather than silently preferring the
    /// unrotated read, so behaviour on a blank page stays predictable.
    #[test]
    fn an_equal_score_keeps_the_detected_orientation() {
        let oriented = OcrResult::new("", 0.0).with_rotation(90);
        let unrotated = OcrResult::new("", 0.0);
        assert_eq!(
            TesseractOcr::better_reading(oriented, unrotated).rotation_degrees,
            90
        );
    }
}

#[cfg(not(feature = "native-tesseract"))]
#[derive(Clone, Debug, Default)]
pub struct TesseractOcr;

#[cfg(not(feature = "native-tesseract"))]
impl TesseractOcr {
    pub fn new(
        _executable: impl Into<PathBuf>,
        _tessdata_directory: impl Into<PathBuf>,
    ) -> Result<Self, ExtractionError> {
        Err(ExtractionError::native_assets_missing(
            "intern-worker was built without the native-tesseract feature",
        ))
    }
}

#[cfg(not(feature = "native-tesseract"))]
impl OcrBackend for TesseractOcr {
    fn recognize(
        &self,
        _page: &RenderedPage,
        _cancel: &CancellationToken,
    ) -> Result<OcrResult, ExtractionError> {
        Err(ExtractionError::native_assets_missing(
            "Tesseract support is unavailable",
        ))
    }
}
