use std::time::{Duration, Instant};

pub(crate) const FRAME_OBSERVATION_TIME: Duration = Duration::from_millis(750);
const MIN_VISIBLE_SAMPLES: usize = 3;

#[derive(Default)]
pub(crate) struct FrameObservation {
    visible_since: Option<Instant>,
    samples: usize,
}

impl FrameObservation {
    pub(crate) fn observe(&mut self, visible: bool, now: Instant) -> bool {
        if !visible {
            *self = Self::default();
            return false;
        }
        let since = *self.visible_since.get_or_insert(now);
        self.samples += 1;
        self.samples >= MIN_VISIBLE_SAMPLES && now.duration_since(since) >= FRAME_OBSERVATION_TIME
    }
}

pub(crate) fn app_error_text_marker(text: &str) -> Option<&'static str> {
    let normalized = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    APP_ERROR_TEXT_MARKERS
        .iter()
        .copied()
        .find(|marker| normalized.contains(marker))
}

pub(crate) fn find_ocr_text_matches(tsv: &str, text: &str) -> Vec<(i32, i32)> {
    let needle = normalized_ocr_words(text);
    if needle.is_empty() {
        return Vec::new();
    }
    let words = parse_ocr_words(tsv);
    words
        .chunk_by(|left, right| left.is_same_line(right))
        .flat_map(|line| find_ocr_text_matches_in_line(line, &needle))
        .collect()
}

fn find_ocr_text_matches_in_line(line: &[OcrWord], needle: &[String]) -> Vec<(i32, i32)> {
    line.windows(needle.len())
        .filter(|window| {
            window
                .iter()
                .map(|word| word.text.as_str())
                .eq(needle.iter().map(String::as_str))
        })
        .map(center_of_words)
        .collect()
}

fn parse_ocr_words(tsv: &str) -> Vec<OcrWord> {
    tsv.lines()
        .skip(1)
        .filter_map(|line| {
            let columns: Vec<_> = line.split('\t').collect();
            let text = columns.get(11)?.trim();
            if text.is_empty() {
                return None;
            }
            let text = normalize_ocr_word(text);
            if text.is_empty() {
                return None;
            }
            Some(OcrWord {
                text,
                block_num: columns.get(2)?.parse().ok()?,
                par_num: columns.get(3)?.parse().ok()?,
                line_num: columns.get(4)?.parse().ok()?,
                left: columns.get(6)?.parse().ok()?,
                top: columns.get(7)?.parse().ok()?,
                width: columns.get(8)?.parse().ok()?,
                height: columns.get(9)?.parse().ok()?,
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OcrWord {
    text: String,
    block_num: i32,
    par_num: i32,
    line_num: i32,
    left: i32,
    top: i32,
    width: i32,
    height: i32,
}

impl OcrWord {
    fn is_same_line(&self, other: &Self) -> bool {
        self.block_num == other.block_num
            && self.par_num == other.par_num
            && self.line_num == other.line_num
    }
}

fn center_of_words(words: &[OcrWord]) -> (i32, i32) {
    let left = words.iter().map(|word| word.left).min().unwrap_or_default();
    let top = words.iter().map(|word| word.top).min().unwrap_or_default();
    let right = words
        .iter()
        .map(|word| word.left + word.width)
        .max()
        .unwrap_or_default();
    let bottom = words
        .iter()
        .map(|word| word.top + word.height)
        .max()
        .unwrap_or_default();
    ((left + right) / 2, (top + bottom) / 2)
}

fn normalized_ocr_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(normalize_ocr_word)
        .filter(|word| !word.is_empty())
        .collect()
}

fn normalize_ocr_word(text: &str) -> String {
    text.trim_matches(|ch: char| !ch.is_alphanumeric())
        .to_ascii_lowercase()
}

pub(crate) fn ocr_text_contains_label(ocr_text: &str, label: &str) -> bool {
    let haystack = normalized_ocr_words(ocr_text);
    let needle = normalized_ocr_words(label);
    if needle.is_empty() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_slice())
}

const APP_ERROR_TEXT_MARKERS: &[&str] = &[
    "secret portal error",
    "unexpected error",
    "fatal error",
    "unhandled exception",
    "application error",
    "something went wrong",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_observation_requires_persistent_content_and_multiple_samples() {
        let started = Instant::now();
        let mut observation = FrameObservation::default();
        assert!(!observation.observe(true, started));
        assert!(!observation.observe(true, started + Duration::from_millis(400)));
        assert!(!observation.observe(false, started + Duration::from_millis(600)));
        assert!(!observation.observe(true, started + Duration::from_millis(800)));
        assert!(!observation.observe(true, started + Duration::from_millis(1200)));
        assert!(observation.observe(true, started + Duration::from_millis(1600)));

        observation.observe(false, started + Duration::from_secs(2));
        assert!(!observation.observe(true, started + Duration::from_secs(3)));

        let mut observation = FrameObservation::default();
        assert!(!observation.observe(true, started));
        assert!(!observation.observe(true, started + FRAME_OBSERVATION_TIME));
        assert!(observation.observe(
            true,
            started + FRAME_OBSERVATION_TIME + Duration::from_millis(200)
        ));

        let mut observation = FrameObservation::default();
        for millis in [0, 100, 200, 300] {
            assert!(!observation.observe(true, started + Duration::from_millis(millis)));
        }
        assert!(observation.observe(true, started + FRAME_OBSERVATION_TIME));
    }

    #[test]
    fn finds_ocr_label_center_and_error_markers() {
        let tsv = concat!(
            "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n",
            "5\t1\t1\t1\t1\t1\t100\t50\t40\t20\t96\tLog\n",
            "5\t1\t1\t1\t1\t2\t148\t50\t24\t20\t96\tIn\n",
            "5\t1\t1\t1\t2\t1\t10\t90\t40\t20\t96\tOther\n",
        );
        assert_eq!(find_ocr_text_matches(tsv, "Log In"), vec![(136, 60)]);
        assert!(find_ocr_text_matches(tsv, "Sign In").is_empty());
        assert_eq!(
            app_error_text_marker("Secret\nPortal   Error"),
            Some("secret portal error")
        );
        assert_eq!(
            app_error_text_marker("An unexpected error occurred"),
            Some("unexpected error")
        );
        assert_eq!(app_error_text_marker("Error handling preferences"), None);
        assert_eq!(app_error_text_marker("Preferences"), None);
    }

    #[test]
    fn label_matching_uses_normalized_consecutive_words() {
        assert!(ocr_text_contains_label("q Log In >", "Log In"));
        assert!(ocr_text_contains_label(": Click Me :", "Click Me"));
        assert!(!ocr_text_contains_label("Advanced", "Log In"));
    }
}
