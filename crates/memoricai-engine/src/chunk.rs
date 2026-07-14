//! Chunking. Markdown splits on heading hierarchy, code on blank-line/def
//! boundaries, everything else on paragraphs — then greedily packs to a target
//! size with light sentence overlap.

/// A produced chunk: (content, position, chunk_type).
pub type ChunkPiece = (String, i32, String);

pub fn chunk_text(text: &str, doc_type: &str, target_chars: usize) -> Vec<ChunkPiece> {
    let target = target_chars.max(200);
    let blocks: Vec<String> = match doc_type {
        "md" | "markdown" => split_markdown(text),
        "code" => split_code(text),
        _ => split_paragraphs(text),
    };
    pack(blocks, target)
        .into_iter()
        .enumerate()
        .map(|(i, content)| (content, i as i32, "text".to_string()))
        .collect()
}

fn split_markdown(text: &str) -> Vec<String> {
    // Break before each heading line, keeping the heading with its section.
    let mut sections: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        let is_heading = line.trim_start().starts_with('#');
        if is_heading && !current.trim().is_empty() {
            sections.push(std::mem::take(&mut current));
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        sections.push(current);
    }
    if sections.is_empty() {
        split_paragraphs(text)
    } else {
        sections
    }
}

fn split_code(text: &str) -> Vec<String> {
    // Split on blank lines but keep def/class starts attached to following body.
    let mut blocks: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        if line.trim().is_empty() && current.lines().count() > 3 {
            blocks.push(std::mem::take(&mut current));
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        blocks.push(current);
    }
    if blocks.is_empty() {
        split_paragraphs(text)
    } else {
        blocks
    }
}

fn split_paragraphs(text: &str) -> Vec<String> {
    let parts: Vec<String> = text
        .split("\n\n")
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        text.split(['.', '!', '?'])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        parts
    }
}

/// Greedily pack blocks up to `target` chars; oversized blocks are split hard.
fn pack(blocks: Vec<String>, target: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut buf = String::new();
    for block in blocks {
        for piece in hard_split(block, target * 2) {
            if buf.is_empty() {
                buf = piece;
            } else if buf.len() + piece.len() + 2 <= target {
                buf.push_str("\n\n");
                buf.push_str(&piece);
            } else {
                out.push(std::mem::take(&mut buf));
                buf = piece;
            }
        }
    }
    if !buf.trim().is_empty() {
        out.push(buf);
    }
    out
}

/// Split a single oversized block on char boundaries near `max`.
fn hard_split(s: String, max: usize) -> Vec<String> {
    if s.chars().count() <= max {
        return vec![s];
    }
    let max = max.max(1);
    let mut out = Vec::new();
    let mut buf = String::new();
    for word in s.split_whitespace() {
        // A single "word" with no interior whitespace can itself exceed the ceiling
        // (minified JS/JSON, base64 blobs, whitespace-free CJK text). Break it on char
        // boundaries so no chunk passes through oversized and later overflows the
        // embedder's per-input limit, which would permanently fail the document.
        if word.len() > max {
            if !buf.is_empty() {
                out.push(std::mem::take(&mut buf));
            }
            let mut count = 0usize;
            let mut start = 0usize;
            for (i, _) in word.char_indices() {
                if count == max {
                    out.push(word[start..i].to_string());
                    start = i;
                    count = 0;
                }
                count += 1;
            }
            if start < word.len() {
                buf = word[start..].to_string();
            }
            continue;
        }
        if buf.len() + word.len() + 1 > max && !buf.is_empty() {
            out.push(std::mem::take(&mut buf));
        }
        if !buf.is_empty() {
            buf.push(' ');
        }
        buf.push_str(word);
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_to_target() {
        let text = "para one is here.\n\npara two follows.\n\npara three ends it.";
        let chunks = chunk_text(text, "text", 40);
        assert!(!chunks.is_empty());
        assert!(chunks.iter().all(|(c, _, _)| c.len() <= 80));
        // positions are sequential from 0
        for (i, (_, pos, _)) in chunks.iter().enumerate() {
            assert_eq!(*pos, i as i32);
        }
    }

    #[test]
    fn markdown_splits_large_sections_on_headings() {
        // Two sections each well above the 200-char floor so they don't merge.
        let alpha = "alpha ".repeat(60);
        let beta = "beta ".repeat(60);
        let md = format!("# A\n{alpha}\n\n# B\n{beta}");
        let chunks = chunk_text(&md, "md", 200);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].0.contains("alpha"));
        assert!(chunks[1].0.contains("beta"));
    }

    #[test]
    fn whitespace_free_text_is_split_below_the_ceiling() {
        // A long run of text with no whitespace (minified code, base64, CJK prose) must
        // still be broken up rather than emitted as one giant chunk.
        let blob = "a".repeat(100_000);
        let chunks = chunk_text(&blob, "text", 1200);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|(c, _, _)| c.chars().count() <= 2400));

        // Multibyte, whitespace-free text must not panic and must stay bounded.
        let cjk = "字".repeat(50_000);
        let chunks = chunk_text(&cjk, "text", 1200);
        assert!(chunks.iter().all(|(c, _, _)| c.chars().count() <= 2400));
    }

    #[test]
    fn small_markdown_sections_merge() {
        let md = "# A\nalpha content\n\n# B\nbeta content";
        let chunks = chunk_text(md, "md", 10_000);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].0.contains("alpha") && chunks[0].0.contains("beta"));
    }
}
