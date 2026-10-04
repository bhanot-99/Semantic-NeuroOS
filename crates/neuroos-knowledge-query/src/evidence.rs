//! P5-S03 / FR-KNO-03: evidence retrieval for a question
//! (`QueryHybridVectorText`, top_k = 5 default). C3 fuses vector and
//! keyword search (BUG-007(b)); this drops duplicate chunk text and chunks
//! that are neither close to the question nor a strong keyword match
//! (KPI-1 diagnosis 2026-09-29: repeated window-focus events filled top_k
//! with identical titles, and irrelevant chunks made the model ramble
//! instead of saying "I don't know").
//!
//! BUG-007: questions about the shape of the user's time ("what did I do
//! most", "the last episode", "before X") aren't answered by any single
//! chunk. They are routed to C3's structured `QueryActivity` instead,
//! keeping the evidence to a handful of records (TimelineQA, Tan et al.
//! 2023: aggregate QA works on small, exact evidence sets and collapses on
//! large ones), and every line carries its local time, oldest first
//! (chronological evidence order helps temporal QA: ChronoRAG, 2025).
use std::collections::HashSet;

use neuroos_proto::v1::{ActivityItem, ChunkMatch};

use crate::storage_client::{StorageClient, StorageClientError};

/// FR-KNO-03: "Retrieve evidence from C3 (top_k = 5 default)."
pub const DEFAULT_TOP_K: u32 = 5;

/// Candidates fetched from C3 per returned chunk, so dropping duplicates
/// still leaves `DEFAULT_TOP_K` distinct chunks when they exist.
const OVERFETCH_FACTOR: u32 = 4;

/// Chunks farther than this from the question are not evidence, unless
/// they are a strong keyword match (below).
pub const MAX_EVIDENCE_DISTANCE: f32 = 0.775;

/// BUG-007(b): a chunk outside the distance cutoff still counts as
/// evidence when it contains more than this many distinct question terms
/// (C3's `keyword_score`): one shared word ("test" for "unit tests") is
/// coincidence, two ("lush" + "pop") name the activity.
pub const MIN_KEYWORD_SCORE: f32 = 1.0;

/// Records per structured-activity answer (TimelineQA: keep it small).
const ACTIVITY_LIMIT: u32 = 6;

/// "Before/after X" looks this far from X's first occurrence.
const NEIGHBOURHOOD_NS: u64 = 60 * 60 * 1_000_000_000;

/// What kind of evidence a question needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// A specific thing: fused vector + keyword retrieval.
    Specific,
    /// "What did I do most / what projects": time spent per title.
    Overview,
    /// "The last/latest X": retrieval, newest first.
    Latest,
    /// "Before X" / "after X": what was on screen around X's first occurrence.
    Around { before: bool, anchor: String },
}

const OVERVIEW_CUES: &[&str] = &[
    "most of",
    "mostly",
    "most time",
    "spend",
    "spent",
    "whole evening",
    "all evening",
    "all day",
    "what projects",
    "which projects",
    "what was i doing",
    "what have i been doing",
    "busy with",
];
const LATEST_CUES: &[&str] = &["last ", "latest", "most recent", "recently"];
const MEDIA_CUES: &[&str] = &[
    "watch", "episode", "video", "movie", "show", "listen", "song", "music", "played", "youtube",
];

/// Lowercase keyword routing; deliberately simple and conservative: any
/// question not clearly about time-shape stays `Specific`.
pub fn classify(question: &str) -> Intent {
    let q = question.to_lowercase();
    for (cue, before) in [(" before ", true), (" after ", false)] {
        if let Some((_, rest)) = q.split_once(cue) {
            let anchor = rest.trim_end_matches(['?', '.', '!']).trim();
            if !anchor.is_empty() {
                return Intent::Around {
                    before,
                    anchor: anchor.to_string(),
                };
            }
        }
    }
    if OVERVIEW_CUES.iter().any(|c| q.contains(c)) {
        return Intent::Overview;
    }
    if LATEST_CUES.iter().any(|c| q.contains(c)) {
        return Intent::Latest;
    }
    Intent::Specific
}

fn is_about_work(question: &str) -> bool {
    let q = question.to_lowercase();
    ["project", "work", "build", "code"]
        .iter()
        .any(|c| q.contains(c))
}

/// Media players and video pages: time spent there isn't project work.
fn is_media_window(w: &ActivityItem) -> bool {
    let app = w.app_id.to_lowercase();
    ["vlc", "mpv", "totem", "celluloid", "spotify"]
        .iter()
        .any(|p| app.contains(p))
        || w.text.contains("YouTube")
}

fn mentions_media(question: &str) -> bool {
    let q = question.to_lowercase();
    MEDIA_CUES.iter().any(|c| q.contains(c))
}

pub async fn retrieve(
    client: &StorageClient,
    question: &str,
) -> Result<Vec<ChunkMatch>, StorageClientError> {
    match classify(question) {
        Intent::Overview => {
            let a = client.query_activity(0, 0, ACTIVITY_LIMIT * 3).await?;
            let mut windows = a.windows;
            if is_about_work(question) {
                windows.retain(|w| !is_media_window(w));
            }
            windows.truncate(ACTIVITY_LIMIT as usize);
            Ok(overview_evidence(&windows))
        }
        Intent::Around { before, anchor } => {
            let hits = specific(client, &anchor).await?;
            let Some(t) = hits.iter().map(|c| c.t_ns).filter(|&t| t > 0).min() else {
                return Ok(Vec::new());
            };
            let (since, until) = if before {
                (t.saturating_sub(NEIGHBOURHOOD_NS), t.saturating_sub(1))
            } else {
                (t + 1, t + NEIGHBOURHOOD_NS)
            };
            let a = client.query_activity(since, until, ACTIVITY_LIMIT).await?;
            Ok(timestamped(chronological(activity_chunks(&a.windows))))
        }
        Intent::Latest => {
            let mut chunks = with_media(client, question).await?;
            chunks.sort_by_key(|c| std::cmp::Reverse(c.t_ns));
            Ok(timestamped(chunks))
        }
        Intent::Specific => {
            // Best match last, right before the question: small models
            // attend most to the end of the context ("lost in the
            // middle", Liu et al. 2023). Media history (less specific)
            // goes first.
            let mut chunks = with_media(client, question).await?;
            let split = chunks
                .iter()
                .position(|c| c.domain == "activity")
                .unwrap_or(chunks.len());
            let media = chunks.split_off(split);
            chunks.reverse();
            Ok(timestamped(media.into_iter().chain(chunks).collect()))
        }
    }
}

/// Fused retrieval for one query, filtered to real evidence.
async fn specific(
    client: &StorageClient,
    query: &str,
) -> Result<Vec<ChunkMatch>, StorageClientError> {
    let candidates = client
        .query_hybrid(query, DEFAULT_TOP_K * OVERFETCH_FACTOR)
        .await?;
    Ok(select_evidence(candidates, DEFAULT_TOP_K as usize))
}

/// Fused retrieval, plus the media history when the question is about
/// watching/listening and everything it names is actually in the record.
async fn with_media(
    client: &StorageClient,
    question: &str,
) -> Result<Vec<ChunkMatch>, StorageClientError> {
    let mut chunks = specific(client, question).await?;
    if mentions_media(question) {
        let a = client.query_activity(0, 0, ACTIVITY_LIMIT).await?;
        let known: Vec<&str> = chunks
            .iter()
            .map(|c| c.text.as_str())
            .chain(a.media.iter().map(|m| m.text.as_str()))
            .collect();
        if names_are_known(question, &known) {
            merge_media(&mut chunks, &relevant_media(question, &a.media));
        }
    }
    Ok(chunks)
}

/// Capitalized words after the first ("Netflix", "YouTube"), other than
/// "I": the specific services/titles a question names.
fn proper_nouns(question: &str) -> Vec<String> {
    question
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .skip(1)
        .filter(|w| *w != "I" && w.chars().next().is_some_and(char::is_uppercase))
        .map(str::to_lowercase)
        .collect()
}

/// Negative-control guard: "What did I watch on Netflix?" must not get
/// the unrelated media history as evidence when Netflix appears nowhere.
fn names_are_known(question: &str, texts: &[&str]) -> bool {
    let lower: Vec<String> = texts.iter().map(|t| t.to_lowercase()).collect();
    proper_nouns(question)
        .iter()
        .all(|n| lower.iter().any(|t| t.contains(n.as_str())))
}

fn activity_chunk(text: String, t_ns: u64) -> ChunkMatch {
    ChunkMatch {
        chunk_id: String::new(),
        entity_id: 0,
        text,
        taint: None,
        t_ns,
        domain: "activity".to_string(),
        distance: 0.0,
        keyword_score: 0.0,
    }
}

fn minutes(ms: u64) -> u64 {
    (ms + 30_000) / 60_000
}

/// "Most time" evidence: one line per title with its total focused time,
/// largest first (already ordered by C3).
fn overview_evidence(windows: &[ActivityItem]) -> Vec<ChunkMatch> {
    windows
        .iter()
        .map(|w| {
            activity_chunk(
                format!(
                    "{} min total: {}: {}",
                    minutes(w.dwell_ms),
                    w.app_id,
                    w.text
                ),
                w.first_ns,
            )
        })
        .collect()
}

fn activity_chunks(windows: &[ActivityItem]) -> Vec<ChunkMatch> {
    windows
        .iter()
        .map(|w| activity_chunk(format!("{}: {}", w.app_id, w.text), w.first_ns))
        .collect()
}

/// Words too generic to tie a question to one media item.
const GENERIC_WORDS: &[&str] = &[
    "what", "which", "who", "did", "the", "and", "was", "were", "watch", "watched", "watching",
    "video", "videos", "episode", "episodes", "show", "listen", "played", "made", "posted",
];

/// The media items sharing a distinctive word with the question ("Lush",
/// "IELTS", "Korean"); all of them when none do. Fewer, on-topic lines:
/// the small model picks the wrong item from a long list.
fn relevant_media(question: &str, media: &[ActivityItem]) -> Vec<ActivityItem> {
    let words: Vec<String> = question
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| w.len() > 2 && !GENERIC_WORDS.contains(&w.as_str()))
        .collect();
    let on_topic: Vec<ActivityItem> = media
        .iter()
        .filter(|m| {
            let t = m.text.to_lowercase();
            words.iter().any(|w| t.contains(w.as_str()))
        })
        .cloned()
        .collect();
    if on_topic.is_empty() {
        media.to_vec()
    } else {
        on_topic
    }
}

/// Appends media not already in `chunks` (by text), keeping it small.
fn merge_media(chunks: &mut Vec<ChunkMatch>, media: &[ActivityItem]) {
    let have: HashSet<String> = chunks.iter().map(|c| c.text.clone()).collect();
    for m in media {
        if chunks.len() >= DEFAULT_TOP_K as usize + 3 {
            break;
        }
        if !have.contains(&m.text) {
            chunks.push(activity_chunk(m.text.clone(), m.first_ns));
        }
    }
}

fn chronological(mut chunks: Vec<ChunkMatch>) -> Vec<ChunkMatch> {
    chunks.sort_by_key(|c| c.t_ns);
    chunks
}

/// Prefixes each chunk with its local wall-clock time.
fn timestamped(chunks: Vec<ChunkMatch>) -> Vec<ChunkMatch> {
    chunks
        .into_iter()
        .map(|mut c| {
            if c.t_ns > 0 {
                c.text = format!("[{}] {}", neuroos_common::time::local_hhmm(c.t_ns), c.text);
            }
            c
        })
        .collect()
}

/// Input is best-first (C3's fused rank). Keeps the first chunk per
/// distinct text, drops chunks that are neither within
/// [`MAX_EVIDENCE_DISTANCE`] nor a keyword match stronger than
/// [`MIN_KEYWORD_SCORE`], and caps the result at `top_k`.
fn select_evidence(candidates: Vec<ChunkMatch>, top_k: usize) -> Vec<ChunkMatch> {
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter(|c| c.distance <= MAX_EVIDENCE_DISTANCE || c.keyword_score > MIN_KEYWORD_SCORE)
        .filter(|c| seen.insert(c.text.clone()))
        .take(top_k)
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    fn chunk(text: &str, distance: f32) -> ChunkMatch {
        ChunkMatch {
            chunk_id: String::new(),
            entity_id: 0,
            text: text.to_string(),
            taint: None,
            t_ns: 0,
            domain: "window_focus".to_string(),
            distance,
            keyword_score: 0.0,
        }
    }

    #[test]
    fn duplicates_are_dropped_keeping_the_closest() {
        let out = select_evidence(vec![chunk("a", 0.1), chunk("a", 0.2), chunk("b", 0.3)], 5);
        let texts: Vec<_> = out.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["a", "b"]);
        assert!((out[0].distance - 0.1).abs() < f32::EPSILON);
    }

    #[test]
    fn chunks_beyond_the_distance_cutoff_are_dropped() {
        let out = select_evidence(vec![chunk("near", 0.5), chunk("far", 0.9)], 5);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "near");
    }

    #[test]
    fn result_is_capped_at_top_k() {
        let cands = (0..10).map(|i| chunk(&format!("c{i}"), 0.1)).collect();
        assert_eq!(select_evidence(cands, 5).len(), 5);
    }

    #[test]
    fn all_far_yields_empty() {
        assert!(select_evidence(vec![chunk("x", 0.95)], 5).is_empty());
    }

    #[test]
    fn classify_routes_time_shaped_questions() {
        assert_eq!(
            classify("What was I doing most of the evening?"),
            Intent::Overview
        );
        assert_eq!(classify("What projects did I work on?"), Intent::Overview);
        assert_eq!(
            classify("What did I do in the Trash folder?"),
            Intent::Specific
        );
        assert_eq!(
            classify("What was the last episode I watched?"),
            Intent::Latest
        );
        assert_eq!(
            classify("What was I doing before watching the gangster show?"),
            Intent::Around {
                before: true,
                anchor: "watching the gangster show".into()
            }
        );
        assert_eq!(
            classify("Who made the lush-pop repository?"),
            Intent::Specific
        );
    }

    #[test]
    fn keyword_only_chunks_need_two_question_terms() {
        let mut one = chunk("-c: Test", 2.0);
        one.keyword_score = 1.0;
        let mut two = chunk("VaughnValle/lush-pop", 2.0);
        two.keyword_score = 2.0;
        let out = select_evidence(vec![one, two], 5);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "VaughnValle/lush-pop");
    }

    #[test]
    fn overview_lines_carry_rounded_minutes() {
        let w = ActivityItem {
            app_id: "vlc".into(),
            text: "Show E03".into(),
            first_ns: 1,
            last_ns: 2,
            dwell_ms: 29 * 60_000 + 40_000,
        };
        assert_eq!(
            overview_evidence(&[w])[0].text,
            "30 min total: vlc: Show E03"
        );
    }

    #[test]
    fn media_history_only_joins_when_every_named_service_is_known() {
        let known = [
            "brave played \"IELTS Task 1\" by IELTS Advantage",
            "vlc played \"Show\"",
        ];
        assert!(names_are_known(
            "What resolution were the episodes I watched?",
            &known
        ));
        assert!(names_are_known(
            "Which YouTube channel made the IELTS videos?",
            &["YouTube - Brave", known[0]]
        ));
        assert!(!names_are_known("What did I watch on Netflix?", &known));
        assert!(!names_are_known(
            "What Spotify playlist did I listen to?",
            &known
        ));
    }

    #[test]
    fn media_on_the_questions_topic_is_preferred() {
        let item = |t: &str| ActivityItem {
            text: t.into(),
            ..Default::default()
        };
        let media = [
            item("brave played \"IELTS Task 1\" by IELTS Advantage"),
            item("brave played \"Lush - Make Linux Beautiful\" by #sudo"),
        ];
        let lush = relevant_media(
            "Which YouTube channel posted the Lush video I watched?",
            &media,
        );
        assert_eq!(lush.len(), 1);
        assert!(lush[0].text.contains("Lush"));
        assert_eq!(
            relevant_media("What resolution were the episodes I watched?", &media).len(),
            2
        );
    }

    #[test]
    fn work_overviews_leave_out_media_time() {
        let w = |app: &str, text: &str| ActivityItem {
            app_id: app.into(),
            text: text.into(),
            ..Default::default()
        };
        assert!(is_about_work("What projects did I work on?"));
        assert!(!is_about_work("What was I doing most of the evening?"));
        assert!(is_media_window(&w("vlc", "Show.S01E02 - VLC media player")));
        assert!(is_media_window(&w(
            "brave-browser",
            "IELTS Task 1 - YouTube - Brave"
        )));
        assert!(!is_media_window(&w(
            "com.system76.CosmicTerm",
            "Omarchy Jarvis theme installation"
        )));
    }
}
