//! Binary/media extraction for uploaded files: PDF text, image OCR+captioning
//! (via the Vision provider), and audio/video transcription (via the Transcriber).

use crate::{extract, Engine};
use memoricai_core::error::{Error, Result};

fn has_ext(name: &str, exts: &[&str]) -> bool {
    let l = name.to_lowercase();
    exts.iter().any(|e| l.ends_with(e))
}

impl Engine {
    /// Extract text + a document type from uploaded bytes by mime/extension:
    /// PDF, images (vision), audio (transcription), or UTF-8 text.
    pub async fn extract_file(
        &self,
        bytes: &[u8],
        filename: &str,
        mime: &str,
    ) -> Result<(String, String)> {
        if mime == "application/pdf" || has_ext(filename, &[".pdf"]) {
            let bytes = bytes.to_vec();
            let text = tokio::task::spawn_blocking(move || extract::extract_pdf_bytes(&bytes))
                .await
                .map_err(|error| {
                    Error::Internal(format!("PDF extraction task failed: {error}"))
                })??;
            return Ok((text, "pdf".to_string()));
        }

        if mime.starts_with("image/")
            || has_ext(filename, &[".png", ".jpg", ".jpeg", ".webp", ".gif"])
        {
            return match &self.models.vision {
                Some(v) => {
                    let caption = v
                        .caption(
                            bytes,
                            if mime.is_empty() { "image/png" } else { mime },
                            "Describe this image in detail and transcribe any visible text (OCR).",
                        )
                        .await?;
                    Ok((caption, "image".to_string()))
                }
                None => Err(Error::BadRequest(
                    "no vision model configured for image uploads (set MEMORICAI_VISION_BASE_URL)"
                        .into(),
                )),
            };
        }

        let is_audio = mime.starts_with("audio/")
            || has_ext(filename, &[".mp3", ".wav", ".m4a", ".ogg", ".flac"]);
        let is_video =
            mime.starts_with("video/") || has_ext(filename, &[".mp4", ".mov", ".webm", ".mkv"]);
        if is_audio || is_video {
            return match &self.models.transcriber {
                Some(t) => {
                    let text = t
                        .transcribe(
                            bytes,
                            filename,
                            if mime.is_empty() { "audio/mpeg" } else { mime },
                        )
                        .await?;
                    Ok((
                        text,
                        if is_video {
                            "video".to_string()
                        } else {
                            "audio".to_string()
                        },
                    ))
                }
                None => Err(Error::BadRequest(
                    "no transcriber configured for audio/video (set MEMORICAI_TRANSCRIBE_BASE_URL)"
                        .into(),
                )),
            };
        }

        // Fallback: treat as UTF-8 text.
        let text = String::from_utf8(bytes.to_vec())
            .map_err(|_| Error::BadRequest("unsupported binary file type".into()))?;
        Ok((text, "text".to_string()))
    }
}
