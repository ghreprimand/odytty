// SPDX-License-Identifier: GPL-3.0-only
//! Small dependency-free fuzzy subsequence scorer.
//!
//! This module is intentionally presentation-agnostic: it knows nothing about
//! overlays, terminal state, PTYs, or history files. The Phase 3 command
//! palette can feed it actions, directories, or history rows and receive stable
//! best-first candidate indexes.

use std::cmp::Ordering;

/// Higher scores are better.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Score(i32);

impl Score {
    /// Raw score value for diagnostics and tests.
    pub fn get(self) -> i32 {
        self.0
    }
}

/// Matching options.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MatchOptions {
    /// When true, characters must match exactly. When false, matching is
    /// case-insensitive but exact-case positions still receive a small bonus.
    pub case_sensitive: bool,
}

#[derive(Debug, Clone, Copy)]
struct CandidateChar {
    ch: char,
    folded: char,
    boundary: Boundary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Boundary {
    None,
    Start,
    Word,
    Camel,
}

/// Score `candidate` against `query` with default case-insensitive matching.
///
/// Returns `None` when `query` is not a subsequence of `candidate`.
pub fn score(query: &str, candidate: &str) -> Option<Score> {
    score_with_options(query, candidate, MatchOptions::default())
}

/// Score `candidate` against `query` with explicit options.
///
/// Empty queries match every candidate. Shorter candidates rank higher for an
/// empty query, which keeps an empty command-palette filter stable and compact.
pub fn score_with_options(query: &str, candidate: &str, options: MatchOptions) -> Option<Score> {
    let query_chars: Vec<char> = query.chars().collect();
    let candidate_chars = candidate_chars(candidate);
    if query_chars.is_empty() {
        return Some(Score(-(candidate_chars.len() as i32)));
    }
    if candidate_chars.is_empty() || query_chars.len() > candidate_chars.len() {
        return None;
    }

    let mut previous: Vec<Option<i32>> = Vec::new();
    for (query_index, query_ch) in query_chars.iter().copied().enumerate() {
        let mut current = vec![None; candidate_chars.len()];
        let query_folded = fold_char(query_ch);

        for (candidate_index, candidate_ch) in candidate_chars.iter().enumerate() {
            if !chars_match(query_ch, query_folded, *candidate_ch, options) {
                continue;
            }

            let char_score = character_score(query_ch, *candidate_ch, candidate_index);
            if query_index == 0 {
                current[candidate_index] = Some(char_score);
                continue;
            }

            let best_previous = previous
                .iter()
                .take(candidate_index)
                .enumerate()
                .filter_map(|(previous_index, previous_score)| {
                    let previous_score = (*previous_score)?;
                    let gap = candidate_index - previous_index - 1;
                    let transition = if gap == 0 {
                        CONSECUTIVE_BONUS
                    } else {
                        -(gap as i32 * GAP_PENALTY)
                    };
                    Some(previous_score + transition)
                })
                .max();

            if let Some(best_previous) = best_previous {
                current[candidate_index] = Some(best_previous + char_score);
            }
        }

        previous = current;
    }

    previous
        .into_iter()
        .enumerate()
        .filter_map(|(last_index, score)| {
            let score = score?;
            let trailing = candidate_chars.len() - last_index - 1;
            let adjusted =
                score - trailing as i32 * TRAILING_PENALTY - candidate_chars.len() as i32;
            Some(Score(adjusted))
        })
        .max()
}

/// Rank candidate indexes best-first with default case-insensitive matching.
///
/// Ties preserve input order.
pub fn rank<S: AsRef<str>>(query: &str, candidates: &[S]) -> Vec<(usize, Score)> {
    rank_with_options(query, candidates, MatchOptions::default())
}

/// Rank candidate indexes best-first with explicit matching options.
///
/// Ties preserve input order.
pub fn rank_with_options<S: AsRef<str>>(
    query: &str,
    candidates: &[S],
    options: MatchOptions,
) -> Vec<(usize, Score)> {
    let mut ranked: Vec<(usize, Score)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            score_with_options(query, candidate.as_ref(), options).map(|score| (index, score))
        })
        .collect();
    ranked.sort_by(|left, right| match right.1.cmp(&left.1) {
        Ordering::Equal => Ordering::Equal,
        ordering => ordering,
    });
    ranked
}

const MATCH_BASE: i32 = 100;
const START_BONUS: i32 = 80;
const WORD_BONUS: i32 = 60;
const CAMEL_BONUS: i32 = 50;
const EXACT_CASE_BONUS: i32 = 10;
const CONSECUTIVE_BONUS: i32 = 75;
const GAP_PENALTY: i32 = 12;
const EARLY_PENALTY: i32 = 4;
const TRAILING_PENALTY: i32 = 1;

fn candidate_chars(candidate: &str) -> Vec<CandidateChar> {
    let chars: Vec<char> = candidate.chars().collect();
    chars
        .iter()
        .copied()
        .enumerate()
        .map(|(index, ch)| {
            let previous = index
                .checked_sub(1)
                .and_then(|prev| chars.get(prev).copied());
            CandidateChar {
                ch,
                folded: fold_char(ch),
                boundary: boundary_for(previous, ch, index),
            }
        })
        .collect()
}

fn fold_char(ch: char) -> char {
    ch.to_lowercase().next().unwrap_or(ch)
}

fn chars_match(
    query: char,
    query_folded: char,
    candidate: CandidateChar,
    options: MatchOptions,
) -> bool {
    if options.case_sensitive {
        query == candidate.ch
    } else {
        query_folded == candidate.folded
    }
}

fn character_score(query: char, candidate: CandidateChar, index: usize) -> i32 {
    MATCH_BASE + boundary_bonus(candidate.boundary) + exact_case_bonus(query, candidate.ch)
        - index as i32 * EARLY_PENALTY
}

fn boundary_bonus(boundary: Boundary) -> i32 {
    match boundary {
        Boundary::None => 0,
        Boundary::Start => START_BONUS,
        Boundary::Word => WORD_BONUS,
        Boundary::Camel => CAMEL_BONUS,
    }
}

fn exact_case_bonus(query: char, candidate: char) -> i32 {
    if query == candidate {
        EXACT_CASE_BONUS
    } else {
        0
    }
}

fn boundary_for(previous: Option<char>, current: char, index: usize) -> Boundary {
    if index == 0 {
        return Boundary::Start;
    }
    let Some(previous) = previous else {
        return Boundary::None;
    };
    if is_word_separator(previous) {
        return Boundary::Word;
    }
    if previous.is_lowercase() && current.is_uppercase() {
        return Boundary::Camel;
    }
    Boundary::None
}

fn is_word_separator(ch: char) -> bool {
    matches!(
        ch,
        '/' | '\\' | '_' | '-' | '.' | ' ' | '\t' | ':' | ';' | ','
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indexes(ranked: &[(usize, Score)]) -> Vec<usize> {
        ranked.iter().map(|(index, _)| *index).collect()
    }

    #[test]
    fn non_subsequence_returns_none() {
        assert_eq!(score("abc", "acb"), None);
        assert_eq!(score("z", "alpha"), None);
    }

    #[test]
    fn consecutive_run_beats_scattered_match() {
        let candidates = ["a-b-c", "abc"];
        assert_eq!(indexes(&rank("abc", &candidates)), vec![1, 0]);
    }

    #[test]
    fn word_boundary_beats_mid_word_match() {
        let candidates = ["xxfbar", "foo-bar"];
        assert_eq!(indexes(&rank("fb", &candidates)), vec![1, 0]);
    }

    #[test]
    fn camel_boundary_beats_mid_word_match() {
        let candidates = ["foob", "fooBar"];
        assert_eq!(indexes(&rank("fb", &candidates)), vec![1, 0]);
    }

    #[test]
    fn prefix_beats_suffix_match() {
        let candidates = ["my-app", "app"];
        assert_eq!(indexes(&rank("app", &candidates)), vec![1, 0]);
    }

    #[test]
    fn shorter_candidate_beats_longer_at_equal_match_quality() {
        let candidates = ["application", "app"];
        assert_eq!(indexes(&rank("app", &candidates)), vec![1, 0]);
    }

    #[test]
    fn empty_query_matches_everything_and_prefers_shorter_candidates() {
        let candidates = ["abcd", "", "a"];
        let ranked = rank("", &candidates);
        assert_eq!(indexes(&ranked), vec![1, 2, 0]);
        assert_eq!(ranked.len(), candidates.len());
    }

    #[test]
    fn case_exact_match_ranks_above_case_folded_match() {
        let candidates = ["ab", "Ab"];
        assert_eq!(indexes(&rank("Ab", &candidates)), vec![1, 0]);
    }

    #[test]
    fn case_sensitive_option_rejects_folded_match() {
        let options = MatchOptions {
            case_sensitive: true,
        };
        assert_eq!(score_with_options("Ab", "ab", options), None);
        assert!(score_with_options("Ab", "Ab", options).is_some());
    }

    #[test]
    fn rank_filters_non_matches_and_preserves_input_order_on_ties() {
        let candidates = ["ab", "zz", "ab"];
        let ranked = rank("ab", &candidates);
        assert_eq!(indexes(&ranked), vec![0, 2]);
        assert_eq!(ranked[0].1, ranked[1].1);
    }
}
