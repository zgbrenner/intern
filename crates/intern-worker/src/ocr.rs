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
use crate::extract::apply_detected_rotation;
use crate::extract::{CancellationToken, ExtractionError, OcrBackend, OcrResult, RenderedPage};
#[cfg(feature = "native-tesseract")]
use crate::limits::ResourceLimits;
#[cfg(feature = "native-tesseract")]
use crate::temp::TempWorkspace;

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

        let rotated = apply_detected_rotation(page.image.clone(), rotation)?;
        let input = self.write_png(&workspace, "rotated.png", &rotated)?;
        let output_base = workspace.path().join("ocr");
        let child = Command::new(&self.executable)
            .arg(&input)
            .arg(&output_base)
            .arg("-l")
            .arg(&self.language)
            .arg("--tessdata-dir")
            .arg(&self.tessdata_directory)
            .arg("--psm")
            .arg("1")
            .arg("tsv")
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
}

#[cfg(all(test, feature = "native-tesseract"))]
mod tests {
    use super::TesseractOcr;
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
