//! Stage 4D: non-expiring recovery holds and payload-only retention. No schema
//! change, no header/edge/counter deletion, and no inference from a missing file.
use super::{ancestry, db::VaultDb};
use fs4::FileExt;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs::{File, OpenOptions},
    path::Path,
};
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RevisionBinding {
    pub vault_uuid: String,
    pub storage_epoch: String,
    pub incarnation_id: String,
    pub revision_id: String,
    pub commit_token: String,
}
impl RevisionBinding {
    pub fn from_accepted(accepted: &Value) -> Result<Self, String> {
        serde_json::from_value(json!({"vaultUuid":accepted["vaultUuid"],"storageEpoch":accepted["storageEpoch"],
            "incarnationId":accepted["revision"]["incarnationId"],"revisionId":accepted["revision"]["revisionId"],"commitToken":accepted["revision"]["commitToken"]})).map_err(error)
    }
    pub fn verify(&self, db: &VaultDb, note: &str, accepted: &Value) -> Result<Value, String> {
        let session = db
            .revision_session
            .as_ref()
            .ok_or("Expected revision vault")?;
        if self.vault_uuid != session.identity.vault_uuid
            || self.storage_epoch != session.identity.storage_epoch
        {
            return Err(
                "Recovery belongs to another vault/restore epoch; explicit enrollment required"
                    .into(),
            );
        }
        for id in [&self.incarnation_id, &self.revision_id, &self.commit_token] {
            uuid::Uuid::parse_str(id).map_err(error)?;
        }
        let receipt = db
            .accepted_at_token(
                note,
                accepted["token"].as_str().ok_or("Missing accepted token")?,
            )
            .map_err(error)?;
        if Self::from_accepted(&receipt["accepted"])? != *self {
            return Err("Recovery revision binding mismatch".into());
        }
        for (field, stored) in [
            ("body", "content"),
            ("title", "title"),
            ("tags", "tags"),
            ("pinned", "pinned"),
            ("trashed", "trashed"),
            ("created", "created"),
            ("modified", "modified"),
        ] {
            if let Some(value) = accepted.get(field) {
                if value != &receipt[stored] {
                    return Err(
                        "Accepted recovery state does not match its immutable revision".into(),
                    );
                }
            }
        }
        Ok(receipt)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct PinRef {
    pub pin_id: String,
    pub revision_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinGroup {
    pub owner: String,
    pub pins: Vec<PinRef>,
}

fn lease_file(root: &Path, instance: &str) -> Result<File, String> {
    uuid::Uuid::parse_str(instance).map_err(error)?;
    let path = root.join(format!("reader-{instance}.lock"));
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err("Unsafe reader lease".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o077 != 0 {
                return Err("Reader lease permissions are not private".into());
            }
        }
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(error)
}
pub(crate) fn session_lease(root: &Path, instance: &str) -> Result<File, String> {
    let file = lease_file(root, instance)?;
    FileExt::try_lock_exclusive(&file).map_err(error)?;
    Ok(file)
}
pub fn pin_session(pin: &PinRef) -> Result<&str, String> {
    let mut parts = pin.pin_id.split(':');
    if !matches!(parts.next(), Some("read" | "base" | "review" | "portable")) {
        return Err("Unknown pin protocol; refusing cleanup".into());
    }
    let session = parts.next().ok_or("Missing pin session")?;
    uuid::Uuid::parse_str(session).map_err(error)?;
    Ok(session)
}
pub fn session_active(db: &VaultDb, session: &str) -> Result<bool, String> {
    let root = &db
        .revision_session
        .as_ref()
        .ok_or("Expected revision vault")?
        .ledger_root;
    let file = lease_file(root, session)?;
    match FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
        Err(e) => Err(error(e)),
    }
}
fn pin_on(
    db: &VaultDb,
    owner: &str,
    kind: &str,
    prefix: &str,
    binding: &RevisionBinding,
) -> Result<(), String> {
    uuid::Uuid::parse_str(owner).map_err(error)?;
    let session = db
        .revision_session
        .as_ref()
        .ok_or("Expected revision vault")?;
    let pin = format!(
        "{prefix}:{}:{owner}:{}",
        session.instance, binding.revision_id
    );
    db.conn.execute("INSERT OR IGNORE INTO revision_pins(pin_id,revision_id,owner_kind,owner_id,created) VALUES(?1,?2,?3,?4,?5)",params![pin,binding.revision_id,kind,owner,chrono::Utc::now().to_rfc3339()]).map_err(error)?;
    Ok(())
}
pub(crate) fn read_is_pinned(db: &VaultDb, note: &Value) -> Result<bool, String> {
    let session = db
        .revision_session
        .as_ref()
        .ok_or("Expected revision vault")?;
    let binding = RevisionBinding::from_accepted(&note["accepted"])?;
    db.conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM revision_pins WHERE pin_id=?1 AND revision_id=?2)",
            params![
                format!(
                    "read:{}:{}:{}",
                    session.instance, session.instance, binding.revision_id
                ),
                binding.revision_id
            ],
            |r| r.get(0),
        )
        .map_err(error)
}
pub(crate) fn pin_read_on(db: &VaultDb, note: &Value) -> Result<(), String> {
    let session = db
        .revision_session
        .as_ref()
        .ok_or("Expected revision vault")?;
    pin_on(
        db,
        &session.instance,
        "recovery",
        "read",
        &RevisionBinding::from_accepted(&note["accepted"])?,
    )
}
impl VaultDb {
    /// Return the exact accepted base with its owner pin committed atomically.
    /// The provisional session read hold bridges probe -> editable owner adoption.
    pub fn pin_owner_base(
        &self,
        owner: &str,
        note: &str,
        accepted: &Value,
    ) -> Result<RevisionBinding, String> {
        let tx = self.begin_revision_maintenance(&json!(["pin-owner", owner, note]))?;
        let binding = RevisionBinding::from_accepted(accepted)?;
        binding.verify(self, note, accepted)?;
        pin_on(self, owner, "recovery", "base", &binding)?;
        tx.commit()?;
        Ok(binding)
    }
    pub fn pinned_owner_read(
        &self,
        owner: &str,
        note: &str,
        accepted: &Value,
    ) -> Result<Value, String> {
        let tx = self.begin_revision_maintenance(&json!(["owner-read", owner, note]))?;
        let binding = RevisionBinding::from_accepted(accepted)?;
        let receipt = binding.verify(self, note, accepted)?;
        pin_on(self, owner, "recovery", "base", &binding)?;
        tx.commit()?;
        #[cfg(test)]
        crate::recovery::crash_boundary("owner-pin");
        Ok(receipt)
    }
    pub fn pin_review(&self, review: &str, note: &str, base: &Value) -> Result<Value, String> {
        let tx = self.begin_revision_maintenance(&json!(["pin-review", review, note]))?;
        let binding = RevisionBinding::from_accepted(base)?;
        binding.verify(self, note, base)?;
        pin_on(self, review, "review", "review", &binding)?;
        let remote = self
            .accepted_note(note)
            .map_err(error)?
            .ok_or("Reviewed note was deleted")?;
        let remote_binding = RevisionBinding::from_accepted(&remote["accepted"])?;
        if remote_binding.incarnation_id != binding.incarnation_id || remote["trashed"] == true {
            return Err("Reviewed note changed lifecycle/incarnation".into());
        }
        pin_on(self, review, "review", "review", &remote_binding)?;
        tx.commit()?;
        #[cfg(test)]
        crate::recovery::crash_boundary("review-pins");
        Ok(remote)
    }
    /// Establish base + all asset holds before reading bytes or publishing a
    /// checkpoint. A subsequent disk failure deliberately leaves these holds.
    pub fn hold_checkpoint(
        &self,
        owner: &str,
        note: &str,
        accepted: &Value,
        assets: &[String],
        review: bool,
    ) -> Result<RevisionBinding, String> {
        let tx = self.begin_revision_maintenance(&json!(["checkpoint-holds", owner]))?;
        let binding = RevisionBinding::from_accepted(accepted)?;
        binding.verify(self, note, accepted)?;
        pin_on(
            self,
            owner,
            if review { "review" } else { "recovery" },
            if review { "review" } else { "base" },
            &binding,
        )?;
        let key = format!("recovery_assets:v3:{owner}");
        let prior: Option<String> = tx
            .query_row("SELECT value FROM meta WHERE key=?1", [&key], |r| r.get(0))
            .optional()
            .map_err(error)?;
        let mut held: BTreeSet<String> = prior
            .map(|s| serde_json::from_str(&s).map_err(error))
            .transpose()?
            .unwrap_or_default();
        for asset in assets {
            let exists: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM attachments WHERE id=?1)",
                    [asset],
                    |r| r.get(0),
                )
                .map_err(error)?;
            if !exists {
                return Err("Checkpoint asset is unavailable; memory must be retained".into());
            }
            held.insert(asset.clone());
        }
        tx.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,serde_json::to_string(&held).map_err(error)?]).map_err(error)?;
        tx.commit()?;
        Ok(binding)
    }
    pub(crate) fn owns_binding(
        &self,
        owner: &str,
        binding: &RevisionBinding,
    ) -> Result<bool, String> {
        self.conn.query_row("SELECT EXISTS(SELECT 1 FROM revision_pins p JOIN revisions r USING(revision_id) WHERE p.owner_id=?1 AND p.revision_id=?2 AND r.incarnation_id=?3 AND r.storage_epoch=?4 AND r.commit_token=?5)",params![owner,binding.revision_id,binding.incarnation_id,binding.storage_epoch,binding.commit_token],|r|r.get(0)).map_err(error)
    }
    pub fn owner_pins(&self, owner: &str) -> Result<Vec<PinRef>, String> {
        let mut statement=self.conn.prepare("SELECT pin_id,revision_id FROM revision_pins WHERE owner_id=?1 ORDER BY pin_id,revision_id").map_err(error)?;
        let rows = statement
            .query_map([owner], |r| {
                Ok(PinRef {
                    pin_id: r.get(0)?,
                    revision_id: r.get(1)?,
                })
            })
            .map_err(error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(error)
    }
    pub fn pin_groups(&self) -> Result<Vec<PinGroup>, String> {
        let mut statement = self
            .conn
            .prepare("SELECT DISTINCT owner_id FROM revision_pins ORDER BY owner_id")
            .map_err(error)?;
        let ids = statement
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        ids.into_iter()
            .map(|owner| {
                Ok(PinGroup {
                    pins: self.owner_pins(&owner)?,
                    owner,
                })
            })
            .collect()
    }
    /// Called only after the native recovery service publishes/fsyncs its
    /// matching immutable tombstone and marker. Check the entire captured set
    /// again under the write lock so a newer checkpoint cannot lose its pins.
    pub(crate) fn release_pins(
        &self,
        owner: &str,
        expected: &[PinRef],
        keep_revision: Option<&str>,
        proof: &crate::recovery::DurableResolution,
    ) -> Result<(), String> {
        if !proof.matches(self, owner, expected, keep_revision) {
            return Err("Missing matching durable resolution proof".into());
        }
        let tx =
            self.begin_revision_maintenance(&json!(["release-pins", owner, proof.digest()]))?;
        if self.owner_pins(owner)?.as_slice() != expected {
            return Err("Pin set changed during resolution; all holds retained".into());
        }
        for pin in expected {
            if Some(pin.revision_id.as_str()) != keep_revision {
                tx.execute(
                    "DELETE FROM revision_pins WHERE pin_id=?1 AND revision_id=?2",
                    params![pin.pin_id, pin.revision_id],
                )
                .map_err(error)?;
            }
        }
        tx.execute(
            "DELETE FROM meta WHERE key=?1",
            [format!("recovery_assets:v3:{owner}")],
        )
        .map_err(error)?;
        tx.commit()
    }
}

#[derive(Default, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GcReport {
    pub states_pruned: u64,
    pub blobs_removed: u64,
}
impl VaultDb {
    /// Fixed approved floor: older than 90 days AND outside newest 100 headers
    /// of this incarnation AND neither head nor pinned. No version setting used.
    pub fn collect_revisions(&self) -> Result<GcReport, String> {
        self.collect_revisions_at(chrono::Utc::now())
    }
    pub(crate) fn collect_revisions_at(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<GcReport, String> {
        if !self.is_v3() {
            return Err("Revision retention requires schema 3".into());
        }
        let tx = self.begin_revision_maintenance(&json!(["retention", now.to_rfc3339()]))?;
        let cutoff = (now - chrono::Duration::days(90)).to_rfc3339();
        let mut report = GcReport::default();
        let eligible="SELECT s.revision_id FROM revision_states s JOIN revisions r USING(revision_id)
            WHERE julianday(r.committed_at)<julianday(?1)
            AND (SELECT COUNT(*) FROM revisions newer WHERE newer.incarnation_id=r.incarnation_id AND newer.revision_no>r.revision_no)>=100
            AND NOT EXISTS(SELECT 1 FROM note_revision_heads h WHERE h.revision_id=r.revision_id)
            AND NOT EXISTS(SELECT 1 FROM revision_pins p WHERE p.revision_id=r.revision_id)";
        let ids = {
            let mut statement = self.conn.prepare(eligible).map_err(error)?;
            let rows = statement
                .query_map([&cutoff], |r| r.get::<_, String>(0))
                .map_err(error)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(error)?
        };
        for id in ids {
            // Re-evaluate every predicate in the destructive statement itself.
            let inserted=tx.execute(&format!("INSERT INTO revision_pruning(revision_id,pruned_at,policy_version) SELECT revision_id,?2,1 FROM ({eligible}) WHERE revision_id=?3"),params![cutoff,now.to_rfc3339(),id]).map_err(error)?;
            if inserted != 0 {
                report.states_pruned+=tx.execute("DELETE FROM revision_states WHERE revision_id=?1 AND NOT EXISTS(SELECT 1 FROM revision_pins WHERE revision_id=?1) AND NOT EXISTS(SELECT 1 FROM note_revision_heads WHERE revision_id=?1)",[&id]).map_err(error)? as u64;
            }
        }
        // Every ownership domain participates. Invalid hold JSON aborts the
        // transaction; missing external files are never interpreted as release.
        report.blobs_removed=tx.execute("DELETE FROM attachments AS a
            WHERE NOT EXISTS(SELECT 1 FROM note_attachments n WHERE n.attachment_id=a.id)
            AND NOT EXISTS(SELECT 1 FROM revision_attachments r WHERE r.attachment_id=a.id)
            AND NOT EXISTS(SELECT 1 FROM versions v WHERE instr(v.content,a.id)>0)
            AND NOT EXISTS(SELECT 1 FROM meta m WHERE (m.key LIKE 'recovery_fork:%' OR m.key LIKE 'review_apply:%' OR m.key LIKE 'pending_summary:%') AND instr(m.value,a.id)>0)
            AND NOT EXISTS(SELECT 1 FROM meta m WHERE m.key LIKE 'attachment_stage:v3:%' AND json_extract(CASE WHEN m.key LIKE 'attachment_stage:v3:%' THEN m.value ELSE '{}' END,'$.attachmentId')=a.id)
            AND NOT EXISTS(SELECT 1 FROM meta m,json_each(CASE WHEN m.key LIKE 'recovery_assets:v3:%' THEN m.value ELSE '[]' END) held WHERE m.key LIKE 'recovery_assets:v3:%' AND held.value=a.id)",[]).map_err(error)? as u64;
        ancestry::validate(&tx)?;
        tx.commit()?;
        Ok(report)
    }
}
