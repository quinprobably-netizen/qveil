//! Lenses: feeds described in words.
//!
//! A lens is a sentence like "slow woodworking with hand tools, no music,
//! longer than 15 minutes". Fixed rules read it (`read`): there is no
//! language model here. What is left over is searched for on YouTube, one
//! search per topic, and everything else (the filtering, the ordering, the
//! reasons shown with each result) happens on this machine.

pub mod rank;
pub mod read;
