//! Model-owned triage, independent notification assessment, and ranking.
//!
//! The durable executor is `sync::triage_worker`. Legacy record types and
//! extraction modules remain for stored-data compatibility; ingest and workers
//! do not execute the retired classifier or deterministic detectors.

pub mod access;
pub mod agent;
pub mod agent_config;
pub mod amazon;
pub mod auth_pairing;
pub mod calendar;
pub mod condense;
pub mod context;
pub mod deadline;
pub mod decision;
pub mod event_identity;
pub mod events;
pub mod extract;
pub mod fye_headline;
pub mod ghost;
pub mod llm;
pub mod login_code;
pub mod memory;
pub mod money;
pub mod notify_llm;
pub mod order_link;
pub mod profile;
pub mod ranking;
pub mod receipt;
pub mod receipt_match;
pub mod rule_infer;
pub mod rule_suggest;
pub mod rules;
pub mod sender_fatigue;
pub mod shipment;
pub mod smart_clear;
pub(crate) mod text;
pub mod unsub_agent;
pub mod unsub_page;

pub use calendar::{CalendarInfo, CalendarKind, detect_calendar};
pub use deadline::DeadlineHit;
pub use receipt::{ReceiptInfo, detect_receipt, recompute_total};
pub use shipment::{
    CarrierTrack, ShipmentInfo, ShipmentStatus, detect_shipment, is_ambiguous_tracking_shape,
};

/// Historical marker still read by migration of pre-agent extraction rows.
pub const NO_BODY_SKIP_MODEL: &str = "skip-no-body";

/// Historical age-skip marker, retained to interpret pre-agent records.
pub const STALE_SKIP_MODEL: &str = "stale-skip";

#[cfg(test)]
mod tests {
    use super::*;

    // ---- prompt copy: no dashes, in either direction ---------------------

    /// Every system prompt the daemon sends from `src/triage`, named by its
    /// module (or, for a prompt kept as a file under `prompts/`, by the file).
    fn every_system_prompt() -> Vec<(&'static str, &'static str)> {
        vec![
            ("prompts/agent-v1.txt", include_str!("prompts/agent-v1.txt")),
            ("prompts/profile-v1.txt", profile::system_prompt()),
            ("fye_headline", fye_headline::SYSTEM_PROMPT),
            ("rule_suggest", rule_suggest::SYSTEM_PROMPT),
            ("smart_clear", smart_clear::SYSTEM_PROMPT),
            ("rule_infer", rule_infer::SYSTEM_PROMPT),
            ("unsub_agent", unsub_agent::SYSTEM_PROMPT),
            ("unsub_page", unsub_page::SYSTEM_PROMPT),
            ("extract::shipments", extract::shipments::SYSTEM_PROMPT),
            ("extract::banking", extract::banking::SYSTEM_PROMPT),
            ("extract::marketing", extract::marketing::SYSTEM_PROMPT),
        ]
    }

    /// Passband's copy carries no em or en dashes, and the prompts must not
    /// USE one either: a prompt that dashes is a style the model reads far
    /// more of than any sentence telling it otherwise. The profile prompt's
    /// fields are shown on the review page, so it also says so outright.
    #[test]
    fn no_system_prompt_models_a_dash() {
        for (name, prompt) in every_system_prompt() {
            assert!(
                !prompt.contains('\u{2014}'),
                "{name}'s prompt uses an em dash"
            );
            assert!(
                !prompt.contains('\u{2013}'),
                "{name}'s prompt uses an en dash"
            );
        }
        assert!(profile::system_prompt().contains("Never write an em dash or en dash"));
    }

    /// The list above is hand-written, so a prompt added later could quietly
    /// skip the check. Every file under `prompts/` and every module under
    /// `src/triage` declaring a `SYSTEM_PROMPT` must be on it.
    #[test]
    fn no_system_prompt_in_the_tree_escapes_the_list() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/triage");
        let listed: Vec<&str> = every_system_prompt().iter().map(|(n, _)| *n).collect();
        for entry in std::fs::read_dir(root.join("prompts")).expect("prompts are readable") {
            let name = entry.expect("an entry").file_name();
            let name = format!("prompts/{}", name.to_string_lossy());
            assert!(
                listed.contains(&name.as_str()),
                "{name} is missing from every_system_prompt()"
            );
        }
        // Assembled rather than written out, so this file does not match itself.
        let declaration = format!("pub const {}: &str", "SYSTEM_PROMPT");
        let mut dirs = vec![root.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).expect("triage sources are readable") {
                let path = entry.expect("a directory entry").path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                if path.extension().is_none_or(|ext| ext != "rs") {
                    continue;
                }
                let src = std::fs::read_to_string(&path).expect("a source file is readable");
                if !src.contains(&declaration) {
                    continue;
                }
                let module = path
                    .strip_prefix(&root)
                    .expect("under src/triage")
                    .with_extension("")
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("::");
                assert!(
                    listed.contains(&module.as_str()),
                    "{module} declares a system prompt but is missing from every_system_prompt()"
                );
            }
        }
    }
}
