'use strict';

/**
 * The release profile for the shipped native binaries.
 *
 * These settings are kept out of Cargo.toml on purpose. build-agent.js hashes the root manifest
 * into the agent's source fingerprint, so any Cargo.toml edit (even a comment) moves the approved
 * agent pins and the release staging then refuses the package. Cargo's CARGO_PROFILE_RELEASE_*
 * overrides apply the same profile from the build scripts, which no pin covers.
 *
 * Measured 2026-11-24 (REPORT.md, ai-account-center/2026-11-24-build-speed): fat LTO with one
 * codegen unit spent the whole build in three whole-program links. On the 5950X (WSL, -j8) a
 * one-file change took 7 m 54 s fat vs 3 m 58 s thin (~half); on the shared 8-core 1070ti (-j2,
 * load 20) it was 14 m 44 s fat vs 14 m 34 s thin — no win, because thin LTO's parallel codegen
 * needs spare cores. Route release builds to a fast, idle machine (5950X or Nas1).
 */
module.exports = {
  // Thin LTO keeps cross-crate inlining and is parallel; fat LTO single-threads the link.
  CARGO_PROFILE_RELEASE_LTO: 'thin',
  // Sixteen units parallelise the pre-LTO codegen; one unit serialised it for no runtime gain here.
  CARGO_PROFILE_RELEASE_CODEGEN_UNITS: '16',
};
