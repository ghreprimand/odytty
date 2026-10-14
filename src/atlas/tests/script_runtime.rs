// SPDX-License-Identifier: GPL-3.0-only
//! Script faces reach complex-script clusters through the same fallback seam as
//! single code points: the static chain first, the runtime resolver only on a
//! chain miss, and the chosen face must map every scalar of the cluster.

use super::*;
use crate::atlas::RuntimeSymbol;
use crate::atlas::cluster::ClusterFace;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

fn primary() -> FontHandle {
    FontHandle::try_from_vec(
        include_bytes!("../../../tests/fixtures/fonts/bidi-mixed.ttf").to_vec(),
    )
    .expect("project-authored Latin, Hebrew and Arabic fixture")
}

fn devanagari() -> Arc<FontHandle> {
    Arc::new(
        FontHandle::try_from_vec(
            include_bytes!(
                "../../../tests/fixtures/fonts/s5b/northern-indic/Devanagari-subset.ttf"
            )
            .to_vec(),
        )
        .expect("Devanagari fixture"),
    )
}

fn sinhala() -> Arc<FontHandle> {
    Arc::new(
        FontHandle::try_from_vec(
            include_bytes!("../../../tests/fixtures/fonts/s5b/sinhala/Sinhala-subset.ttf").to_vec(),
        )
        .expect("Sinhala fixture"),
    )
}

/// A Devanagari consonant and vowel sign, both mapped by the fixture.
const CLUSTER: [char; 2] = ['\u{0915}', '\u{093F}'];

static CHAIN_FACE_CALLS: AtomicUsize = AtomicUsize::new(0);
fn runtime_after_chain(_ch: char) -> RuntimeSymbol {
    CHAIN_FACE_CALLS.fetch_add(1, Ordering::SeqCst);
    RuntimeSymbol::Ready(Some(sinhala()))
}

/// The static chain is consulted first: a script face already in the chain
/// answers, and the runtime resolver is never asked.
#[test]
fn the_static_chain_answers_before_the_runtime_script_resolver() {
    let chain = devanagari();
    let mut atlas = GlyphAtlas::build(&primary(), 20.0);
    atlas.set_fallback_fonts(vec![Arc::clone(&chain)]);
    CHAIN_FACE_CALLS.store(0, Ordering::SeqCst);
    atlas.set_runtime_symbol_resolver(Some(runtime_after_chain));
    match atlas.cluster_face(&primary(), &CLUSTER) {
        ClusterFace::Fallback(face) => assert!(Arc::ptr_eq(&face, &chain)),
        other => panic!("expected the chain face, got {other:?}"),
    }
    assert_eq!(CHAIN_FACE_CALLS.load(Ordering::SeqCst), 0);
}

static RESOLVED_FACE: OnceLock<Arc<FontHandle>> = OnceLock::new();
static RESOLVED_CALLS: AtomicUsize = AtomicUsize::new(0);
fn runtime_resolves(_ch: char) -> RuntimeSymbol {
    RESOLVED_CALLS.fetch_add(1, Ordering::SeqCst);
    RuntimeSymbol::Ready(Some(Arc::clone(RESOLVED_FACE.get_or_init(devanagari))))
}

/// A chain miss reaches the runtime resolver, and the answer is cached per
/// code point for the atlas.
#[test]
fn a_chain_miss_shapes_the_cluster_from_the_runtime_resolved_face() {
    let mut atlas = GlyphAtlas::build(&primary(), 20.0);
    atlas.set_fallback_fonts(Vec::new());
    RESOLVED_CALLS.store(0, Ordering::SeqCst);
    atlas.set_runtime_symbol_resolver(Some(runtime_resolves));
    for _ in 0..2 {
        match atlas.cluster_face(&primary(), &CLUSTER) {
            ClusterFace::Fallback(face) => {
                assert!(Arc::ptr_eq(&face, RESOLVED_FACE.get().expect("resolved")));
            }
            other => panic!("expected the resolved face, got {other:?}"),
        }
    }
    assert_eq!(RESOLVED_CALLS.load(Ordering::SeqCst), 1, "answer cached");
}

static PENDING_CALLS: AtomicUsize = AtomicUsize::new(0);
fn runtime_pending(_ch: char) -> RuntimeSymbol {
    PENDING_CALLS.fetch_add(1, Ordering::SeqCst);
    RuntimeSymbol::Pending
}

/// A face still loading on the worker keeps the cluster pending without
/// caching, so the rebuild that follows asks again.
#[test]
fn a_pending_runtime_answer_keeps_the_cluster_pending_and_is_asked_again() {
    let mut atlas = GlyphAtlas::build(&primary(), 20.0);
    atlas.set_fallback_fonts(Vec::new());
    PENDING_CALLS.store(0, Ordering::SeqCst);
    atlas.set_runtime_symbol_resolver(Some(runtime_pending));
    for _ in 0..2 {
        assert!(matches!(
            atlas.cluster_face(&primary(), &CLUSTER),
            ClusterFace::Pending
        ));
    }
    assert_eq!(PENDING_CALLS.load(Ordering::SeqCst), 2);
}

static MISMATCH_CALLS: AtomicUsize = AtomicUsize::new(0);
fn runtime_wrong_script(_ch: char) -> RuntimeSymbol {
    MISMATCH_CALLS.fetch_add(1, Ordering::SeqCst);
    RuntimeSymbol::Ready(Some(sinhala()))
}

/// A resolved face that lacks a scalar of the cluster is refused, so the owner
/// keeps the per-cell path instead of shaping from a face with holes.
#[test]
fn a_runtime_face_that_does_not_map_every_scalar_is_refused() {
    let mut atlas = GlyphAtlas::build(&primary(), 20.0);
    atlas.set_fallback_fonts(Vec::new());
    atlas.set_runtime_symbol_resolver(Some(runtime_wrong_script));
    assert!(matches!(
        atlas.cluster_face(&primary(), &CLUSTER),
        ClusterFace::Unavailable
    ));
}
