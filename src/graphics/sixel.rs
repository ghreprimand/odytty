//! Sixel DCS payload decoder (stub — implementation lands in packet SX1).
//!
//! Scope: a pure decoder from raw DCS `q` payload bytes to an RGBA image
//! buffer. No parser or renderer wiring lives here; the DCS routing seam is
//! built in the graphics scene packet (G2.1).
