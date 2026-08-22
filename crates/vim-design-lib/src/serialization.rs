//! Serialization of the parametric layer (docs/ARCHITECTURE.md §10).
//!
//! Envelope: magic `VIMD`, little-endian `u32` format version, postcard
//! payload of `(settings, next_id, entities)`. Deterministic: the entity
//! map is a `BTreeMap`, so identical state always serializes to identical
//! bytes (the round-trip tests assert save → load → save byte-identity).
//! Undo/redo stacks and derived state (reverse index, dirty set) are not
//! persisted; the reverse index is rebuilt and validated on load.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::document::{Document, DocumentSettings};
use crate::entity::EntityRecord;
use crate::graph::GraphState;
use crate::id::EntityId;
use crate::status::VimStatus;

/// File magic. Rejecting on mismatch keeps foreign data from being
/// misinterpreted as a (possibly "valid-looking") document.
const MAGIC: &[u8; 4] = b"VIMD";
/// Current format version. Breaking schema changes bump this; additive
/// fields use serde defaults instead (docs/ARCHITECTURE.md §10).
const FORMAT_VERSION: u32 = 1;

/// The persisted parametric state.
#[derive(Debug, Serialize, Deserialize)]
struct Payload {
    settings: DocumentSettings,
    next_id: u64,
    entities: BTreeMap<EntityId, EntityRecord>,
}

/// The persisted next-id counter is *derived* from the entity map
/// (max id + 1), not copied from the live allocator. Rationale: the live
/// counter is monotonic and never rolled back by undo (so ids are never
/// re-issued within a session — docs/ARCHITECTURE.md §3.1), which means
/// it can exceed the highest live id. Persisting the derived value makes
/// `save` a pure function of (settings, entities): identical parametric
/// state always yields identical bytes, and undo-all restores a document
/// that saves byte-identically to the initial save. The tradeoff — an id
/// allocated and deleted before saving may be re-issued after a load —
/// is safe because undo/redo stacks are not persisted, so no reference
/// to the retired id can survive into the loaded document.
fn derived_next_id(entities: &BTreeMap<EntityId, EntityRecord>) -> u64 {
    entities
        .keys()
        .next_back()
        .map_or(1, |id| id.0.saturating_add(1))
}

/// Serialize `document` into the VIMD envelope.
pub(crate) fn save(document: &Document) -> Result<Vec<u8>, VimStatus> {
    let entities = document.graph_ref().entities();
    let payload = Payload {
        settings: document.settings().clone(),
        next_id: derived_next_id(entities),
        entities: entities.clone(),
    };
    let body =
        postcard::to_allocvec(&payload).map_err(|_| VimStatus::SerializationFailed)?;
    let mut bytes = Vec::with_capacity(8usize.saturating_add(body.len()));
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    bytes.extend_from_slice(&body);
    Ok(bytes)
}

/// Deserialize a document from the VIMD envelope. Bad magic or a newer
/// version → `UnsupportedVersion`; anything undecodable or structurally
/// invalid → `MalformedData`. Never panics.
pub(crate) fn load(bytes: &[u8]) -> Result<Document, VimStatus> {
    let magic = bytes.get(0..4).ok_or(VimStatus::UnsupportedVersion)?;
    if magic != MAGIC {
        return Err(VimStatus::UnsupportedVersion);
    }
    let version_bytes: [u8; 4] = bytes
        .get(4..8)
        .and_then(|slice| slice.try_into().ok())
        .ok_or(VimStatus::UnsupportedVersion)?;
    if u32::from_le_bytes(version_bytes) != FORMAT_VERSION {
        return Err(VimStatus::UnsupportedVersion);
    }
    let body = bytes.get(8..).ok_or(VimStatus::MalformedData)?;
    let payload: Payload =
        postcard::from_bytes(body).map_err(|_| VimStatus::MalformedData)?;
    // Rebuilds the reverse index and runs the full structural validation
    // (shape, kinds, existence, acyclicity) — malformed graphs are
    // rejected here rather than crashing later.
    let graph = GraphState::from_entities(payload.entities)?;
    Ok(Document::from_parts(payload.settings, payload.next_id, graph))
}

/// Dev/debug JSON export for diffing documents in tests.
#[cfg(any(test, feature = "json-debug"))]
pub(crate) fn save_json(document: &Document) -> Result<String, VimStatus> {
    let entities = document.graph_ref().entities();
    let payload = Payload {
        settings: document.settings().clone(),
        next_id: derived_next_id(entities),
        entities: entities.clone(),
    };
    // JSON object keys must be strings; remap the id keys.
    let entities: BTreeMap<String, &EntityRecord> = payload
        .entities
        .iter()
        .map(|(id, record)| (id.0.to_string(), record))
        .collect();
    let value = serde_json::json!({
        "settings": payload.settings,
        "next_id": payload.next_id,
        "entities": entities,
    });
    serde_json::to_string_pretty(&value).map_err(|_| VimStatus::SerializationFailed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Command;

    fn sample_document() -> Document {
        let mut doc = Document::new();
        let _ = doc.submit(Command::CreateCylinder {
            center: [1.0, 2.0, 0.0],
            radius: 0.5,
            height: 3.0,
        });
        doc
    }

    #[test]
    fn save_load_save_is_byte_identical() {
        let doc = sample_document();
        let first = doc.save();
        let reloaded = first.as_ref().ok().and_then(|b| Document::load(b).ok());
        let second = reloaded.as_ref().map(Document::save);
        assert!(first.is_ok());
        assert_eq!(second, Some(first));
    }

    #[test]
    fn save_is_deterministic() {
        let a = sample_document().save();
        let b = sample_document().save();
        assert!(a.is_ok());
        assert_eq!(a.ok(), b.ok());
    }

    #[test]
    fn load_rejects_bad_magic_and_version() {
        assert_eq!(Document::load(b"").err(), Some(VimStatus::UnsupportedVersion));
        assert_eq!(
            Document::load(b"NOPE\x01\x00\x00\x00").err(),
            Some(VimStatus::UnsupportedVersion)
        );
        assert_eq!(
            Document::load(b"VIMD\x63\x00\x00\x00").err(),
            Some(VimStatus::UnsupportedVersion)
        );
    }

    #[test]
    fn load_rejects_truncated_and_garbage_payloads() {
        let doc = sample_document();
        let Ok(bytes) = doc.save() else {
            assert!(doc.save().is_ok());
            return;
        };
        // Truncate the payload progressively: must error, never panic.
        for cut in 8..bytes.len() {
            let truncated = bytes.get(0..cut).unwrap_or_default();
            assert!(Document::load(truncated).is_err());
        }
        // Garbage payload after a valid envelope header.
        let mut garbage = b"VIMD\x01\x00\x00\x00".to_vec();
        garbage.extend_from_slice(&[0xFF; 64]);
        assert!(Document::load(&garbage).is_err());
    }

    #[test]
    fn undo_stacks_are_not_persisted() {
        let mut doc = sample_document();
        assert!(doc.can_undo());
        let reloaded = doc.save().ok().and_then(|b| Document::load(&b).ok());
        assert_eq!(reloaded.map(|d| d.can_undo()), Some(false));
        let _ = doc.undo();
    }

    #[test]
    fn loaded_allocator_does_not_collide_with_existing_ids() {
        let doc = sample_document();
        let reloaded = doc.save().ok().and_then(|b| Document::load(&b).ok());
        assert!(reloaded.is_some(), "reload failed");
        let new_id = reloaded.and_then(|mut d| {
            d.submit(Command::CreateControlPoint { position: [0.0; 3] })
                .ok()
                .and_then(|o| o.created_ids.first().copied())
        });
        let max_existing = doc.entities().map(|(id, _)| *id).max();
        assert!(new_id > max_existing);
    }

    #[test]
    fn json_export_is_valid_json() {
        let doc = sample_document();
        let json = doc.save_json();
        let parsed: Option<serde_json::Value> =
            json.ok().and_then(|s| serde_json::from_str(&s).ok());
        assert!(parsed.is_some());
    }
}
