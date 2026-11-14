//! Format 2 portable graph. Markdown is a readable projection; hash-addressed
//! state sidecars are authoritative and are never passed through a Markdown parser.
//! Production activation follows the host-verified 4B–4E gate result.
use super::{ancestry, db::VaultDb, db_export};
use rusqlite::{
    params,
    types::{Value as SqlValue, ValueRef},
    Connection, OptionalExtension,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

const CAPABILITIES: &[&str] = &[
    "immutable-revision-graph-v1",
    "sha256-byte-sidecars-v1",
    "anonymous-recovery-holds-v1",
];
// Exact ordered columns are a format contract, not SQL supplied by an archive.
// Integer cells use decimal strings, including counters beyond JS's safe range.
const TABLES: &str = r#"[
 {"name":"meta","columns":["key","value"],"key":1},
 {"name":"tags","columns":["name"],"key":1},
 {"name":"notes","columns":["id","title","content","pinned","trashed","created","modified"],"key":1},
 {"name":"attachments","columns":["id","filename","mime_type","size","sha256","data","created"],"key":1},
 {"name":"note_tags","columns":["note_id","tag"],"key":2},
 {"name":"note_attachments","columns":["note_id","attachment_id"],"key":2},
 {"name":"session_links","columns":["note_id","session_id","created"],"key":1},
 {"name":"vault_identity","columns":["singleton","vault_uuid","storage_epoch","created"],"key":1},
 {"name":"note_revision_counters","columns":["note_id","high_water"],"key":1},
 {"name":"note_incarnations","columns":["incarnation_id","note_id","born_at","ended_at"],"key":1},
 {"name":"revisions","columns":["revision_id","incarnation_id","revision_no","storage_epoch","commit_token","state_sha256","writer_source","writer_instance","operation_id","operation_item","request_sha256","kind","ancestry","committed_at"],"key":1},
 {"name":"revision_states","columns":["revision_id","body","title","tags_json","pinned","lifecycle","note_created","note_modified","extra_json"],"key":1},
 {"name":"revision_parents","columns":["child_id","role","ordinal","parent_id"],"key":3},
 {"name":"note_revision_heads","columns":["note_id","incarnation_id","revision_id"],"key":1},
 {"name":"revision_attachments","columns":["revision_id","attachment_id"],"key":2},
 {"name":"revision_pins","columns":["pin_id","revision_id","owner_kind","owner_id","created"],"key":2},
 {"name":"revision_pruning","columns":["revision_id","pruned_at","policy_version"],"key":1},
 {"name":"versions","columns":["id","note_id","content","created","ancestry","revision_id"],"key":1}
]"#;
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn specs() -> Result<Vec<Spec>, String> {
    serde_json::from_str(TABLES).map_err(error)
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Spec {
    name: String,
    columns: Vec<String>,
    key: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct Sidecar {
    file: String,
    sha256: String,
    size: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    name: String,
    columns: Vec<String>,
    rows: Vec<Sidecar>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Manifest {
    capabilities: Vec<String>,
    vault_uuid: String,
    storage_epoch: String,
    storage_epochs: Vec<String>,
    tables: Vec<Table>,
    sidecars: Vec<Sidecar>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Cell {
    Null,
    Integer(String),
    Text(String),
    Blob(Sidecar),
}
type Row = Vec<Cell>;
fn uuid(value: &str) -> Result<(), String> {
    if uuid::Uuid::parse_str(value).map_err(error)?.to_string() != value {
        return Err("Noncanonical portable UUID".into());
    }
    Ok(())
}
fn mapped(namespace: &str, domain: &str, source: &str) -> String {
    let bytes: [u8; 32] =
        Sha256::digest(serde_json::to_vec(&(namespace, domain, source)).expect("string tuple"))
            .into();
    let mut id = [0u8; 16];
    id.copy_from_slice(&bytes[..16]);
    id[6] = (id[6] & 0x0f) | 0x40;
    id[8] = (id[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(id).to_string()
}
impl Sidecar {
    fn for_bytes(bytes: &[u8]) -> Self {
        let sha256 = db_export::digest(bytes);
        Self {
            file: format!(".ryuknotes/ancestry/sha256/{sha256}.bin"),
            sha256,
            size: bytes.len() as u64,
        }
    }
    fn validate(&self) -> Result<(), String> {
        if self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self.file != format!(".ryuknotes/ancestry/sha256/{}.bin", self.sha256)
        {
            return Err("Invalid ancestry sidecar identity/path".into());
        }
        Ok(())
    }
    fn read(&self, root: &Path) -> Result<Vec<u8>, String> {
        self.validate()?;
        let path = db_export::checked_path(root, &self.file)?;
        if std::fs::metadata(&path).map_err(error)?.len() != self.size {
            return Err("Ancestry sidecar size mismatch".into());
        }
        let bytes = std::fs::read(path).map_err(error)?;
        if db_export::digest(&bytes) != self.sha256 {
            return Err("Ancestry sidecar hash mismatch".into());
        }
        Ok(bytes)
    }
}
impl Manifest {
    pub(crate) fn validate_capabilities(&self) -> Result<(), String> {
        if !self
            .capabilities
            .iter()
            .map(String::as_str)
            .eq(CAPABILITIES.iter().copied())
        {
            return Err("Unknown ancestry capability; no writes performed".into());
        }
        uuid(&self.vault_uuid)?;
        uuid(&self.storage_epoch)?;
        let mut epochs = BTreeSet::new();
        for epoch in &self.storage_epochs {
            uuid(epoch)?;
            if !epochs.insert(epoch) {
                return Err("Duplicate portable epoch".into());
            }
        }
        if !epochs.contains(&self.storage_epoch) {
            return Err("Missing active portable epoch".into());
        }
        let specs = specs()?;
        if specs.len() != self.tables.len() {
            return Err("Incomplete portable graph tables".into());
        }
        let mut referenced = BTreeSet::new();
        for (spec, table) in specs.iter().zip(&self.tables) {
            if spec.name != table.name || spec.columns != table.columns {
                return Err("Unknown portable table/column capability".into());
            }
            for file in &table.rows {
                file.validate()?;
                referenced.insert(file.clone());
            }
        }
        let mut declared = BTreeSet::new();
        for file in &self.sidecars {
            file.validate()?;
            if !declared.insert(file.clone()) {
                return Err("Duplicate sidecar declaration".into());
            }
        }
        if !referenced.is_subset(&declared) {
            return Err("Undeclared row sidecar".into());
        }
        Ok(())
    }
    pub(crate) fn files(&self) -> Vec<String> {
        self.sidecars.iter().map(|f| f.file.clone()).collect()
    }
    fn request_hash(&self) -> Result<String, String> {
        Ok(db_export::digest(&serde_json::to_vec(self).map_err(error)?))
    }
}
pub(crate) fn probe(index: &Value) -> Result<Manifest, String> {
    if index["format"] != 2 {
        return Err("Unsupported Markdown export format; ancestry cannot be dropped".into());
    }
    let object = index.as_object().ok_or("Invalid export envelope")?;
    if object.keys().any(|key| {
        ![
            "format",
            "ancestry",
            "notes",
            "attachments",
            "versions",
            "meta",
            "tags",
            "sessions",
        ]
        .contains(&key.as_str())
    }) {
        return Err("Unknown export envelope capability".into());
    }
    let manifest: Manifest = serde_json::from_value(index["ancestry"].clone()).map_err(error)?;
    manifest.validate_capabilities()?;
    Ok(manifest)
}

/// Private checkpoint ownership does not become a live owner on another host.
/// Preserve its assets as conservative portable staging holds pending cleanup.
pub(crate) fn portable_metadata(
    input: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    let mut result = BTreeMap::new();
    let mut assets = BTreeSet::new();
    for (key, value) in input {
        if key.starts_with("recovery_assets:") {
            assets.extend(serde_json::from_str::<BTreeSet<String>>(value).map_err(error)?);
        } else if key.starts_with("attachment_stage:") {
            let value: Value = serde_json::from_str(value).map_err(error)?;
            assets.insert(
                value["attachmentId"]
                    .as_str()
                    .ok_or("Invalid staging hold")?
                    .to_owned(),
            );
        } else if !key.starts_with("recovery_fork:") && !key.starts_with("review_apply:") {
            result.insert(key.clone(), value.clone());
        }
    }
    for asset in assets {
        result.insert(
            format!("attachment_stage:v3:portable:{asset}"),
            json!({"attachmentId":asset,"created":"portable-hold"}).to_string(),
        );
    }
    Ok(result)
}
fn read_rows(conn: &Connection, spec: &Spec) -> Result<Vec<Vec<SqlValue>>, String> {
    let query = format!(
        "SELECT {} FROM {} ORDER BY {}",
        spec.columns.join(","),
        spec.name,
        spec.columns[..spec.key].join(",")
    );
    let mut statement = conn.prepare(&query).map_err(error)?;
    let mut rows = statement.query([]).map_err(error)?;
    let mut result = Vec::new();
    while let Some(row) = rows.next().map_err(error)? {
        let mut values = Vec::new();
        for i in 0..spec.columns.len() {
            values.push(match row.get_ref(i).map_err(error)? {
                ValueRef::Null => SqlValue::Null,
                ValueRef::Integer(v) => SqlValue::Integer(v),
                ValueRef::Text(v) => SqlValue::Text(String::from_utf8(v.to_vec()).map_err(error)?),
                ValueRef::Blob(v) => SqlValue::Blob(v.to_vec()),
                ValueRef::Real(_) => {
                    return Err("Floating point cells are not a portable graph capability".into())
                }
            });
        }
        result.push(values);
    }
    Ok(result)
}
fn text(row: &[SqlValue], n: usize) -> Result<&str, String> {
    match row.get(n) {
        Some(SqlValue::Text(v)) => Ok(v),
        _ => Err("Invalid portable text cell".into()),
    }
}
fn integer(row: &[SqlValue], n: usize) -> Result<i64, String> {
    match row.get(n) {
        Some(SqlValue::Integer(v)) => Ok(*v),
        _ => Err("Invalid portable integer cell".into()),
    }
}

pub(crate) fn capture(
    db: &VaultDb,
    publish: &impl Fn(&str, &[u8]) -> Result<(), String>,
) -> Result<Manifest, String> {
    if !db.is_v3() || db.conn.is_autocommit() {
        return Err("Portable ancestry requires the export's coherent snapshot".into());
    }
    db.validate_writer_vault(&db.identity()).map_err(error)?;
    ancestry::validate(&db.conn)?;
    let identity = ancestry::identity(&db.conn)?;
    let mut manifest = Manifest {
        capabilities: CAPABILITIES.iter().map(|s| (*s).into()).collect(),
        vault_uuid: identity.vault_uuid.clone(),
        storage_epoch: identity.storage_epoch.clone(),
        storage_epochs: vec![],
        tables: vec![],
        sidecars: vec![],
    };
    let mut files = BTreeSet::new();
    let mut epochs = BTreeSet::from([identity.storage_epoch]);
    let mut emit = |bytes: &[u8]| -> Result<Sidecar, String> {
        let file = Sidecar::for_bytes(bytes);
        if files.insert(file.clone()) {
            publish(&file.file, bytes)?;
        }
        Ok(file)
    };
    for spec in specs()? {
        let mut rows = read_rows(&db.conn, &spec)?;
        if spec.name == "meta" {
            let meta = rows
                .iter()
                .map(|r| Ok((text(r, 0)?.to_owned(), text(r, 1)?.to_owned())))
                .collect::<Result<BTreeMap<_, _>, String>>()?;
            let mut meta = portable_metadata(&meta)?;
            let mut orphans=db.conn.prepare("SELECT a.id FROM attachments a WHERE NOT EXISTS(SELECT 1 FROM note_attachments n WHERE n.attachment_id=a.id) AND NOT EXISTS(SELECT 1 FROM revision_attachments r WHERE r.attachment_id=a.id) ORDER BY a.id").map_err(error)?;
            let ids = orphans
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(error)?;
            for id in ids {
                let id = id.map_err(error)?;
                meta.insert(
                    format!("attachment_stage:v3:portable:{id}"),
                    json!({"attachmentId":id,"created":"portable-hold"}).to_string(),
                );
            }
            rows = meta
                .into_iter()
                .map(|(k, v)| vec![SqlValue::Text(k), SqlValue::Text(v)])
                .collect();
        } else if spec.name == "revision_pins" {
            let mut anonymous = BTreeMap::new();
            for row in rows {
                let revision = text(&row, 1)?;
                let kind = text(&row, 2)?;
                let owner = mapped(
                    &identity.vault_uuid,
                    "portable-hold",
                    &format!("{kind}:{revision}"),
                );
                anonymous.insert(
                    (revision.to_owned(), kind.to_owned()),
                    vec![
                        SqlValue::Text(format!("portable:{owner}:{owner}:{revision}")),
                        SqlValue::Text(revision.into()),
                        SqlValue::Text(kind.into()),
                        SqlValue::Text(owner),
                        SqlValue::Text("portable-hold".into()),
                    ],
                );
            }
            rows = anonymous.into_values().collect();
            rows.sort_by_key(|r| text(r, 0).unwrap_or("").to_owned());
        }
        let mut table = Table {
            name: spec.name.clone(),
            columns: spec.columns,
            rows: Vec::new(),
        };
        for row in rows {
            if spec.name == "revisions" {
                epochs.insert(text(&row, 3)?.into());
            }
            let cells = row
                .into_iter()
                .map(|v| match v {
                    SqlValue::Null => Ok(Cell::Null),
                    SqlValue::Integer(v) => Ok(Cell::Integer(v.to_string())),
                    SqlValue::Text(v) => Ok(Cell::Text(v)),
                    SqlValue::Blob(v) => Ok(Cell::Blob(emit(&v)?)),
                    SqlValue::Real(_) => Err("Unsupported real cell".into()),
                })
                .collect::<Result<Row, String>>()?;
            table
                .rows
                .push(emit(&serde_json::to_vec(&cells).map_err(error)?)?);
        }
        manifest.tables.push(table);
    }
    manifest.sidecars = files.into_iter().collect();
    manifest.storage_epochs = epochs.into_iter().collect();
    manifest.validate_capabilities()?;
    Ok(manifest)
}

struct Package {
    manifest: Manifest,
    rows: BTreeMap<String, Vec<Vec<SqlValue>>>,
}
impl Package {
    fn load(root: &Path) -> Result<Self, String> {
        let bytes =
            std::fs::read(db_export::checked_path(root, db_export::INDEX)?).map_err(error)?;
        let index: Value = serde_json::from_slice(&bytes).map_err(error)?;
        // This preflight precedes ALL destination, ledger, lock and staging writes.
        let manifest = probe(&index)?;
        let _publication =
            ancestry::Maintenance::shared(&db_export::checked_path(root, db_export::INDEX)?)?;
        if std::fs::read(db_export::checked_path(root, db_export::INDEX)?).map_err(error)? != bytes
        {
            return Err("Export publication changed; retry import".into());
        }
        Self::read_manifest(root, manifest)
    }
    // Caller owns the archive publication lease (shared import / exclusive export).
    fn read_manifest(root: &Path, manifest: Manifest) -> Result<Self, String> {
        manifest.validate_capabilities()?;
        let declared: BTreeSet<_> = manifest.sidecars.iter().cloned().collect();
        let mut referenced = BTreeSet::new();
        let mut tables = BTreeMap::new();
        for table in &manifest.tables {
            let mut result = Vec::new();
            let mut keys = BTreeSet::new();
            let spec = specs()?
                .into_iter()
                .find(|s| s.name == table.name)
                .ok_or("Unknown table")?;
            for file in &table.rows {
                referenced.insert(file.clone());
                let cells: Row = serde_json::from_slice(&file.read(root)?).map_err(error)?;
                if cells.len() != table.columns.len() {
                    return Err("Portable row column count mismatch".into());
                }
                let key = serde_json::to_string(&cells[..spec.key]).map_err(error)?;
                if !keys.insert(key) {
                    return Err("Duplicate immutable portable row identity".into());
                }
                let mut row = Vec::new();
                for cell in cells {
                    row.push(match cell {
                        Cell::Null => SqlValue::Null,
                        Cell::Text(v) => SqlValue::Text(v),
                        Cell::Integer(v) => {
                            let n = v.parse::<i64>().map_err(error)?;
                            if n.to_string() != v {
                                return Err("Noncanonical portable integer".into());
                            }
                            SqlValue::Integer(n)
                        }
                        Cell::Blob(file) => {
                            if !declared.contains(&file) {
                                return Err("Undeclared payload sidecar".into());
                            }
                            referenced.insert(file.clone());
                            SqlValue::Blob(file.read(root)?)
                        }
                    });
                }
                if spec.name == "attachments"
                    && integer(&row, 3)? > super::db::DEFAULT_MAX_ATTACHMENT_BYTES
                {
                    return Err("Portable attachment exceeds 25 MiB".into());
                }
                validate_row(&manifest, &spec, &row)?;
                result.push(row);
            }
            tables.insert(table.name.clone(), result);
        }
        if referenced != declared {
            return Err("Unreferenced or missing portable sidecar".into());
        }
        Ok(Self {
            manifest,
            rows: tables,
        })
    }
    fn materialize(&self, conn: &Connection) -> Result<(), String> {
        conn.pragma_update(None, "foreign_keys", true)
            .map_err(error)?;
        let tx = conn.unchecked_transaction().map_err(error)?;
        tx.execute_batch(super::db::SCHEMA).map_err(error)?;
        tx.execute_batch(ancestry::DDL).map_err(error)?;
        tx.execute_batch("PRAGMA defer_foreign_keys=ON")
            .map_err(error)?;
        for spec in specs()? {
            for row in &self.rows[&spec.name] {
                insert(&tx, &spec, row)?;
            }
        }
        if ancestry::schema_version(&tx)? != 3 {
            return Err("Portable graph is not schema 3".into());
        }
        let identity = ancestry::identity(&tx)?;
        if identity.vault_uuid != self.manifest.vault_uuid
            || identity.storage_epoch != self.manifest.storage_epoch
        {
            return Err("Portable identity projection mismatch".into());
        }
        let epochs = {
            let mut s=tx.prepare("SELECT DISTINCT storage_epoch FROM revisions UNION SELECT storage_epoch FROM vault_identity ORDER BY storage_epoch").map_err(error)?;
            let rows = s.query_map([], |r| r.get::<_, String>(0)).map_err(error)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(error)?
        };
        if epochs != self.manifest.storage_epochs {
            return Err("Portable epoch manifest mismatch".into());
        }
        let layout = super::db_notebooks::Layout::load(&tx)?;
        for notebook in &layout.notebooks {
            super::db_notebooks::relative("", notebook)?;
        }
        for (note, path) in &layout.notes {
            layout.require(path)?;
            let exists: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM notes WHERE id=?1)",
                    [note],
                    |r| r.get(0),
                )
                .map_err(error)?;
            if !exists {
                return Err("Portable notebook assignment has no note".into());
            }
        }
        ancestry::validate(&tx)?;
        // FTS is derived state, rebuilt rather than treated as ancestry.
        tx.execute(
            "INSERT INTO notes_fts(rowid,title,content) SELECT rowid,title,content FROM notes",
            [],
        )
        .map_err(error)?;
        tx.commit().map_err(error)
    }
}
fn validate_row(manifest: &Manifest, spec: &Spec, row: &[SqlValue]) -> Result<(), String> {
    let ids: &[usize] = match spec.name.as_str() {
        "attachments"
        | "note_incarnations"
        | "revision_states"
        | "revision_attachments"
        | "revision_pruning" => &[0],
        "revisions" => &[0, 1, 3, 4, 7],
        "revision_parents" => &[0, 3],
        "note_revision_heads" => &[1, 2],
        _ => &[],
    };
    for column in ids {
        uuid(text(row, *column)?)?;
    }
    if spec.name == "meta" {
        let key = text(row, 0)?;
        if key.starts_with("recovery_assets:")
            || key.starts_with("recovery_fork:")
            || key.starts_with("review_apply:")
        {
            return Err("Private recovery ownership is not a portable capability".into());
        }
    }
    if spec.name == "revision_pins" {
        let revision = text(row, 1)?;
        uuid(revision)?;
        let owner = mapped(
            &manifest.vault_uuid,
            "portable-hold",
            &format!("{}:{revision}", text(row, 2)?),
        );
        if text(row, 3)? != owner
            || text(row, 0)? != format!("portable:{owner}:{owner}:{revision}")
            || text(row, 4)? != "portable-hold"
        {
            return Err("Portable pin must be an anonymous non-live recovery hold".into());
        }
    }
    Ok(())
}
fn insert(conn: &Connection, spec: &Spec, row: &[SqlValue]) -> Result<(), String> {
    let query = format!(
        "INSERT INTO {}({}) VALUES({})",
        spec.name,
        spec.columns.join(","),
        vec!["?"; spec.columns.len()].join(",")
    );
    conn.execute(&query, rusqlite::params_from_iter(row.iter()))
        .map_err(error)?;
    if existing(conn, spec, row)?.as_deref() != Some(row) {
        return Err("Noncanonical portable SQLite cell type/affinity".into());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreMode {
    FullRestore,
    IndependentCopy,
}
impl Package {
    /// Content note IDs/attachment IDs remain stable names inside the new vault
    /// namespace, preserving exact Markdown bytes and hashes. All graph authority
    /// UUIDs and opaque accepted tokens change. IncarnationOrigin is source
    /// provenance, not a foreign key; retain it with an explicit translation map.
    fn independent_copy(&mut self, operation: &str) -> Result<(), String> {
        let namespace = mapped(operation, "copy-vault", &self.manifest.vault_uuid);
        let old_vault = self.manifest.vault_uuid.clone();
        let source_hash = self.manifest.request_hash()?;
        let mut translations: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        let mut translate = |domain: &str, value: &str| -> String {
            translations
                .entry(domain.into())
                .or_default()
                .entry(value.into())
                .or_insert_with(|| mapped(&namespace, domain, value))
                .clone()
        };
        for (name, rows) in &mut self.rows {
            for row in rows {
                let positions: &[(usize, &str)] = match name.as_str() {
                    "vault_identity" => &[(2, "epoch")],
                    "note_incarnations" => &[(0, "incarnation")],
                    "revisions" => &[
                        (0, "revision"),
                        (1, "incarnation"),
                        (3, "epoch"),
                        (4, "commit"),
                        (7, "writer"),
                        (8, "operation"),
                    ],
                    "revision_states" | "revision_attachments" | "revision_pruning" => {
                        &[(0, "revision")]
                    }
                    "revision_parents" => &[(0, "revision"), (3, "revision")],
                    "note_revision_heads" => &[(1, "incarnation"), (2, "revision")],
                    "versions" => &[(0, "version"), (5, "revision")],
                    "revision_pins" => &[(1, "revision")],
                    _ => &[],
                };
                for (column, domain) in positions {
                    if let SqlValue::Text(value) = &row[*column] {
                        row[*column] = SqlValue::Text(translate(domain, value));
                    }
                }
                if name == "vault_identity" {
                    row[1] = SqlValue::Text(namespace.clone());
                }
                if name == "revision_pins" {
                    let revision = text(row, 1)?.to_owned();
                    let kind = text(row, 2)?;
                    let owner = mapped(&namespace, "portable-hold", &format!("{kind}:{revision}"));
                    row[0] = SqlValue::Text(format!("portable:{owner}:{owner}:{revision}"));
                    row[3] = SqlValue::Text(owner);
                }
            }
        }
        // Retry claims describe source operations. They do not grant permission
        // to replay those operations as authoring intents in a different vault.
        let meta = self
            .rows
            .get_mut("meta")
            .ok_or("Missing portable metadata")?;
        meta.retain(|r| {
            !text(r, 0).unwrap_or("").starts_with("revision_operation")
                && !text(r, 0).unwrap_or("").starts_with("portable_import:")
        });
        for row in meta.iter_mut() {
            if text(row, 0)?.starts_with("generated_summary:v3:") {
                row[1] = SqlValue::Text(translate("revision", text(row, 1)?));
            }
            if text(row,0)?.starts_with("notebook_trash:v1:") {
                let mut record: Value = serde_json::from_str(text(row,1)?).map_err(error)?;
                if let Some(revisions) = record["revisions"].as_object_mut() {
                    for revision in revisions.values_mut() {
                        let source = revision.as_str().ok_or("Invalid notebook trash revision")?;
                        *revision = json!(translate("revision",source));
                    }
                }
                row[1] = SqlValue::Text(record.to_string());
            }
        }
        self.manifest.storage_epoch = translate("epoch", &self.manifest.storage_epoch);
        self.manifest.storage_epochs = self
            .manifest
            .storage_epochs
            .iter()
            .map(|e| translate("epoch", e))
            .collect();
        self.manifest.storage_epochs.sort();
        self.manifest.vault_uuid = namespace;
        meta.push(vec![SqlValue::Text(format!("portable_origin:v1:{operation}")),SqlValue::Text(json!({"sourceVaultUuid":old_vault,"sourceArchiveHash":source_hash,"graphTranslation":translations,"noteAndAttachmentNamesPreserved":true}).to_string())]);
        Ok(())
    }
}

/// A re-export may advance projections and prune eligible payloads, but cannot
/// silently replace an immutable identity or erase a previously exported graph.
/// Choose a new folder for an older restored lineage or another logical vault.
pub(crate) fn validate_reexport(
    root: &Path,
    previous: &Manifest,
    db: &VaultDb,
) -> Result<(), String> {
    if previous.vault_uuid != ancestry::identity(&db.conn)?.vault_uuid {
        return Err("Export folder belongs to another vault UUID; choose a new folder".into());
    }
    let previous = Package::read_manifest(root, previous.clone())?;
    for spec in specs()? {
        if !matches!(
            spec.name.as_str(),
            "revisions"
                | "revision_parents"
                | "revision_states"
                | "revision_pruning"
                | "attachments"
                | "versions"
                | "note_incarnations"
        ) {
            continue;
        }
        for row in &previous.rows[&spec.name] {
            match existing(&db.conn,&spec,row)? {
                Some(current)=>{
                    let matches=if spec.name=="note_incarnations" {current[..3]==row[..3] && (row[3]==SqlValue::Null || current[3]==row[3])} else {current==*row};
                    if !matches {return Err(format!("Immutable {} identity changed since the previous export",spec.name));}
                },
                None if matches!(spec.name.as_str(),"revisions"|"revision_parents"|"revision_pruning"|"note_incarnations")=>return Err("Previously exported ancestry is absent from this lineage; preserve that archive and choose a new folder".into()),
                None=>{}, // Curated history, payloads and unowned bytes may be pruned.
            }
        }
    }
    let known: BTreeSet<_> = previous.rows["revisions"]
        .iter()
        .map(|r| text(r, 0).map(str::to_owned))
        .collect::<Result<_, _>>()?;
    let current = specs()?
        .into_iter()
        .find(|s| s.name == "revision_parents")
        .ok_or("Missing parent capability")?;
    let mut old_edges = BTreeMap::<String, Vec<Vec<SqlValue>>>::new();
    let mut edges = BTreeMap::<String, Vec<Vec<SqlValue>>>::new();
    for row in &previous.rows["revision_parents"] {
        old_edges
            .entry(text(row, 0)?.into())
            .or_default()
            .push(row.clone());
    }
    for row in read_rows(&db.conn, &current)? {
        if known.contains(text(&row, 0)?) {
            edges.entry(text(&row, 0)?.into()).or_default().push(row);
        }
    }
    if old_edges != edges {
        return Err("Immutable parent set changed since previous export".into());
    }
    Ok(())
}

/// Explicit path-based bootstrap/restore boundary, not an authoring backdoor.
/// Validate all capabilities/hashes/graph before acquiring destination or ledger
/// locks. Live destination connections exclude replacement through maintenance.
pub fn restore_export(
    root: &Path,
    destination: &Path,
    ledger_root: &Path,
    mode: RestoreMode,
    operation: &str,
) -> Result<ancestry::RestoreOutcome, String> {
    uuid(operation)?;
    let root = std::fs::canonicalize(root).map_err(error)?;
    let mut package = Package::load(&root)?;
    if let Ok(metadata) = std::fs::symlink_metadata(destination) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err("Portable restore destination must be a regular file or absent".into());
        }
    }
    let target_version = ancestry::inspect_version(destination)?;
    if !matches!(target_version, 0 | 2 | 3) {
        return Err("Unsupported portable restore destination schema".into());
    }
    let request = db_export::digest(
        &serde_json::to_vec(&(package.manifest.request_hash()?, mode)).map_err(error)?,
    );
    if matches!(mode, RestoreMode::IndependentCopy) {
        package.independent_copy(operation)?;
    }
    let stage = tempfile::tempdir().map_err(error)?;
    let path = stage.path().join("portable.rn");
    let conn = Connection::open(&path).map_err(error)?;
    package.materialize(&conn)?;
    drop(conn);
    let key = format!("portable_import:v1:{operation}");
    ancestry::restore_portable(destination, &path, ledger_root, &key, &request)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileOutcome {
    archive_hash: String,
    headers_added: u64,
    requires_recovery_review: bool,
}
fn existing(
    conn: &Connection,
    spec: &Spec,
    row: &[SqlValue],
) -> Result<Option<Vec<SqlValue>>, String> {
    let query = format!(
        "SELECT {} FROM {} WHERE {}",
        spec.columns.join(","),
        spec.name,
        spec.columns[..spec.key]
            .iter()
            .map(|c| format!("{c}=?"))
            .collect::<Vec<_>>()
            .join(" AND ")
    );
    let mut statement = conn.prepare(&query).map_err(error)?;
    statement
        .query_row(rusqlite::params_from_iter(row[..spec.key].iter()), |r| {
            (0..spec.columns.len())
                .map(|i| r.get::<_, SqlValue>(i))
                .collect()
        })
        .optional()
        .map_err(error)
}
fn collision(conn: &Connection, spec: &Spec, row: &[SqlValue]) -> Result<bool, String> {
    if spec.name == "revisions" {
        let mut statement=conn.prepare("SELECT revision_id FROM revisions WHERE commit_token=?1 OR (incarnation_id=?2 AND revision_no=?3) OR (storage_epoch=?4 AND operation_id=?5 AND operation_item=?6)").map_err(error)?;
        let ids = statement
            .query_map(
                params![
                    text(row, 4)?,
                    text(row, 1)?,
                    integer(row, 2)?,
                    text(row, 3)?,
                    text(row, 8)?,
                    text(row, 9)?
                ],
                |r| r.get::<_, String>(0),
            )
            .map_err(error)?;
        for id in ids {
            if id.map_err(error)? != text(row, 0)? {
                return Err("Immutable revision token/counter/operation identity collision".into());
            }
        }
    }
    if spec.name == "attachments" {
        let alias: Option<String> = conn
            .query_row(
                "SELECT id FROM attachments WHERE sha256=?1",
                [text(row, 4)?],
                |r| r.get(0),
            )
            .optional()
            .map_err(error)?;
        if alias
            .as_deref()
            .is_some_and(|id| id != text(row, 0).unwrap_or(""))
        {
            return Err(
                "Immutable attachment hash alias collision; graph references cannot be spliced"
                    .into(),
            );
        }
    }
    if let Some(prior) = existing(conn, spec, row)? {
        if prior != row {
            return Err(format!(
                "Immutable {} identity collision; no graph rows imported",
                spec.name
            ));
        }
        Ok(true)
    } else {
        Ok(false)
    }
}
/// Separate explicit reconciliation: retain the current projection/head and
/// import matching immutable ancestors/branches only. Incoming heads and mutable
/// metadata remain recorded provenance, not permission to overwrite dirty notes.
/// Cross-vault graphs, hash aliases and payload/pruning contradictions fail closed.
pub fn reconcile_export(
    root: &Path,
    db: &VaultDb,
    operation: &str,
) -> Result<ReconcileOutcome, String> {
    uuid(operation)?;
    if !db.is_v3() {
        return Err("Ancestry reconciliation requires a writable schema 3 session".into());
    }
    let root = std::fs::canonicalize(root).map_err(error)?;
    let package = Package::load(&root)?;
    // Complete source validation is mandatory even when the operation is a retry.
    let source = Connection::open_in_memory().map_err(error)?;
    package.materialize(&source)?;
    if package.manifest.vault_uuid
        != db
            .revision_session
            .as_ref()
            .ok_or("Missing revision session")?
            .identity
            .vault_uuid
    {
        return Err("Cannot reconcile another vault UUID; use independent copy".into());
    }
    let hash = package.manifest.request_hash()?;
    let key = format!("portable_reconcile:v1:{operation}");
    let high = package.rows["note_revision_counters"]
        .iter()
        .map(|r| integer(r, 1))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .max()
        .unwrap_or(0);
    let tx = db.begin_portable_reconcile(&json!([operation, hash]), high)?;
    let prior: Option<String> = tx
        .query_row("SELECT value FROM meta WHERE key=?1", [&key], |r| r.get(0))
        .optional()
        .map_err(error)?;
    if let Some(prior) = prior {
        let prior: Value = serde_json::from_str(&prior).map_err(error)?;
        if prior["archiveHash"] != hash {
            return Err("Reconciliation operation ID reused for different input".into());
        }
        let receipt = serde_json::from_value(prior).map_err(error)?;
        tx.commit()?;
        return Ok(receipt);
    }
    let specs = specs()?;
    // Validate every immutable collision before adding anything, including edge
    // set changes that a header/state hash alone cannot detect.
    for spec in &specs {
        if !matches!(
            spec.name.as_str(),
            "attachments"
                | "note_incarnations"
                | "revisions"
                | "revision_states"
                | "revision_parents"
                | "revision_attachments"
                | "revision_pruning"
                | "versions"
        ) {
            continue;
        }
        for row in &package.rows[&spec.name] {
            collision(&tx, spec, row)?;
            if spec.name == "revisions" {
                let id = text(row, 0)?;
                let exists: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM revisions WHERE revision_id=?1)",
                        [id],
                        |r| r.get(0),
                    )
                    .map_err(error)?;
                if exists {
                    let edges = |conn: &Connection| -> Result<Vec<(String, String, i64)>, String> {
                        let mut q=conn.prepare("SELECT parent_id,role,ordinal FROM revision_parents WHERE child_id=?1 ORDER BY role,ordinal").map_err(error)?;
                        let rows = q
                            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                            .map_err(error)?;
                        rows.collect::<Result<_, _>>().map_err(error)
                    };
                    if edges(&tx)? != edges(&source)? {
                        return Err("Immutable revision parent set collision".into());
                    }
                }
            }
            if spec.name == "revision_states" {
                let pruned: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM revision_pruning WHERE revision_id=?1)",
                        [text(row, 0)?],
                        |r| r.get(0),
                    )
                    .map_err(error)?;
                if pruned {
                    return Err(
                        "Reconciliation cannot resurrect a pruned payload; use independent copy"
                            .into(),
                    );
                }
            }
            if spec.name == "revision_pruning" {
                let retained: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM revision_states WHERE revision_id=?1)",
                        [text(row, 0)?],
                        |r| r.get(0),
                    )
                    .map_err(error)?;
                if retained {
                    return Err(
                        "Reconciliation pruning conflicts with a retained local payload".into(),
                    );
                }
            }
        }
    }
    for row in &package.rows["note_revision_counters"] {
        let note = text(row, 0)?;
        let high = integer(row, 1)?;
        let prior: Option<i64> = tx
            .query_row(
                "SELECT high_water FROM note_revision_counters WHERE note_id=?1",
                [note],
                |r| r.get(0),
            )
            .optional()
            .map_err(error)?;
        if prior.is_none() {
            return Err("Reconciliation cannot introduce an unrelated note projection; use full restore/copy".into());
        }
        tx.execute(
            "UPDATE note_revision_counters SET high_water=MAX(high_water,?2) WHERE note_id=?1",
            params![note, high],
        )
        .map_err(error)?;
    }
    let mut receipt = ReconcileOutcome {
        archive_hash: hash.clone(),
        headers_added: 0,
        requires_recovery_review: true,
    };
    tx.execute_batch("PRAGMA defer_foreign_keys=ON")
        .map_err(error)?;
    for spec in &specs {
        if !matches!(
            spec.name.as_str(),
            "attachments"
                | "note_incarnations"
                | "revisions"
                | "revision_states"
                | "revision_parents"
                | "revision_attachments"
                | "revision_pruning"
                | "versions"
                | "revision_pins"
        ) {
            continue;
        }
        for row in &package.rows[&spec.name] {
            if !collision(&tx, spec, row)? {
                insert(&tx, spec, row)?;
                if spec.name == "revisions" {
                    receipt.headers_added += 1;
                }
            }
        }
    }
    // Incoming branch heads remain inspectable and protected without changing
    // any live owner or silently replacing the window's accepted projection.
    for row in &package.rows["note_revision_heads"] {
        let revision = text(row, 2)?;
        let owner = mapped(&package.manifest.vault_uuid, "reconcile-review", &hash);
        tx.execute(
            "INSERT OR IGNORE INTO revision_pins VALUES(?1,?2,'review',?3,'portable-hold')",
            params![
                format!("portable:{owner}:{owner}:{revision}"),
                revision,
                owner
            ],
        )
        .map_err(error)?;
    }
    let incoming_meta = package.rows["meta"]
        .iter()
        .map(|r| Ok((text(r, 0)?.to_owned(), text(r, 1)?.to_owned())))
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    tx.execute("INSERT INTO meta(key,value) VALUES(?1,?2)",params![format!("portable_branch:v1:{hash}"),json!({"sourceVaultUuid":package.manifest.vault_uuid,"sourceStorageEpoch":package.manifest.storage_epoch,"heads":package.rows["note_revision_heads"].iter().map(|r|Ok((text(r,0)?,text(r,1)?,text(r,2)?))).collect::<Result<Vec<_>,String>>()?,"metadata":incoming_meta}).to_string()]).map_err(error)?;
    for row in &package.rows["attachments"] {
        let id = text(row, 0)?;
        tx.execute(
            "INSERT OR IGNORE INTO meta(key,value) VALUES(?1,?2)",
            params![
                format!("attachment_stage:v3:portable:{hash}:{id}"),
                json!({"attachmentId":id,"created":"portable-hold"}).to_string()
            ],
        )
        .map_err(error)?;
    }
    tx.execute(
        "INSERT INTO meta(key,value) VALUES(?1,?2)",
        params![key, serde_json::to_string(&receipt).map_err(error)?],
    )
    .map_err(error)?;
    ancestry::validate(&tx)?;
    tx.commit()?;
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::super::{
        db::NewNote,
        revision_retention::PinGroup,
        writer::{NoteWrite, WritePolicy},
    };
    use super::*;
    struct Fixture {
        dir: tempfile::TempDir,
        path: std::path::PathBuf,
        ledger: std::path::PathBuf,
        out: std::path::PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("source.rn");
            let ledger = dir.path().join("source-ledger");
            let out = dir.path().join("export");
            ancestry::migrate(&path, &ledger).unwrap();
            Self {
                dir,
                path,
                ledger,
                out,
            }
        }
        fn open(&self) -> VaultDb {
            let db = VaultDb::open_v3(&self.path, &self.ledger).unwrap();
            db.conn.pragma_update(None, "journal_mode", "WAL").unwrap();
            db
        }
    }
    fn write(db: &VaultDb, id: &str, body: &str, prior: Option<&Value>) -> Value {
        let note = NewNote {
            id: id.into(),
            title: "雪 title".into(),
            content: body.into(),
            tags: vec!["tag".into()],
            pinned: true,
            created: "2020-01-01T00:00:00Z".into(),
            modified: "2020-01-01T00:00:00Z".into(),
        };
        let policy = prior
            .map(|p| WritePolicy::Update {
                expected_token: p["accepted"]["token"].as_str().unwrap().into(),
            })
            .unwrap_or(WritePolicy::Create);
        db.write_authoritative(&NoteWrite::from_note(db, &note, policy), 0)
            .unwrap()
    }
    fn directory(root: &Path) -> BTreeMap<String, Vec<u8>> {
        walkdir::WalkDir::new(root)
            .into_iter()
            .map(Result::unwrap)
            .filter(|e| e.file_type().is_file())
            .map(|e| {
                (
                    e.path()
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    std::fs::read(e.path()).unwrap(),
                )
            })
            .collect()
    }
    fn rewrite_package(root: &Path, mut package: Package) {
        let mut files = BTreeSet::new();
        let mut emit = |bytes: &[u8]| {
            let file = Sidecar::for_bytes(bytes);
            db_export::write(root, &file.file, bytes).unwrap();
            files.insert(file.clone());
            file
        };
        for table in &mut package.manifest.tables {
            table.rows.clear();
            for row in &package.rows[&table.name] {
                let cells = row
                    .iter()
                    .map(|v| match v {
                        SqlValue::Null => Cell::Null,
                        SqlValue::Integer(v) => Cell::Integer(v.to_string()),
                        SqlValue::Text(v) => Cell::Text(v.clone()),
                        SqlValue::Blob(v) => Cell::Blob(emit(v)),
                        SqlValue::Real(_) => panic!("real"),
                    })
                    .collect::<Row>();
                table.rows.push(emit(&serde_json::to_vec(&cells).unwrap()));
            }
        }
        package.manifest.sidecars = files.into_iter().collect();
        let path = root.join(db_export::INDEX);
        let mut index: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        index["ancestry"] = json!(package.manifest);
        std::fs::write(path, serde_json::to_vec(&index).unwrap()).unwrap();
    }
    #[test]
    fn notebook_trash_restores_after_full_or_independent_portable_restore() {
        let f = Fixture::new();
        let db = f.open();
        let vault = f.path.to_str().unwrap();
        write(&db,"n","exact body\r\n",None);
        crate::vault::db_notebooks::create(&db,vault,None,"Work/Child").unwrap();
        crate::vault::db_notebooks::move_note(&db,vault,"n","Work/Child").unwrap();
        crate::vault::db_notebooks::trash(&db,vault,"Work",&uuid::Uuid::new_v4().to_string()).unwrap();
        db_export::export_md_vault(&f.out,&db).unwrap();
        for (name,mode) in [("restored",RestoreMode::FullRestore),("independent",RestoreMode::IndependentCopy)] {
            let path = f.dir.path().join(format!("{name}.rn"));
            let ledger = f.dir.path().join(format!("{name}-ledger"));
            restore_export(&f.out,&path,&ledger,mode,&uuid::Uuid::new_v4().to_string()).unwrap();
            let copy = VaultDb::open_v3(&path,&ledger).unwrap();
            let group = crate::vault::db_notebooks::trash_groups(&copy).unwrap().remove(0);
            crate::vault::db_notebooks::resolve_trash(&copy,path.to_str().unwrap(),&group.path,false,&uuid::Uuid::new_v4().to_string()).unwrap();
            assert_eq!(copy.accepted_note("n").unwrap().unwrap()["trashed"],false);
            assert_eq!(copy.get_note_content("n").unwrap().unwrap(),"exact body\r\n");
            assert!(crate::vault::db_notebooks::Layout::load(&copy.conn).unwrap().notebooks.contains("Work/Child"));
        }
    }
    #[test]
    fn exact_graph_states_metadata_shared_bytes_pins_pruning_and_restore_round_trip_together() {
        let f = Fixture::new();
        let db = f.open();
        let base = write(&db, "n", " \r\n雪\t\n", None);
        let image = db
            .add_attachment("n", "image.png", "image/png", b"\x89PNG\r\n", "2020")
            .unwrap();
        let file = db
            .add_attachment(
                "n",
                "shared.pdf",
                "application/pdf",
                b"%PDF\0shared",
                "2020",
            )
            .unwrap();
        let body = format!(
            " \r\n雪\t\n![image](.rn-attachments/{})\r\n[file](.rn-attachments/{})\n",
            image.id, file.id
        );
        write(&db, "n", &body, Some(&base));
        let other = write(
            &db,
            "other",
            &format!("[shared](.rn-attachments/{})", file.id),
            None,
        );
        db.set_session_link("n", "session-fixture").unwrap();
        let mut head = db.accepted_note("n").unwrap().unwrap();
        let pin = uuid::Uuid::new_v4().to_string();
        db.pin_owner_base(&pin, "n", &head["accepted"]).unwrap();
        let old_instance = db.revision_session.as_ref().unwrap().instance.clone();
        for n in 0..105 {
            head = write(&db, "n", &format!("{body}revision {n}\r\n"), Some(&head));
        }
        db.set_note_trashed("other", true).unwrap();
        let _ = other;
        drop(db);
        let db = f.open();
        let mut recovery = crate::recovery::Recovery::default();
        recovery
            .set_root(f.dir.path().join("private-recovery"))
            .unwrap();
        recovery
            .cleanup_pin_group(
                &db,
                &PinGroup {
                    owner: old_instance.clone(),
                    pins: db.owner_pins(&old_instance).unwrap(),
                },
            )
            .unwrap();
        assert!(
            db.collect_revisions_at(chrono::Utc::now() + chrono::Duration::days(120))
                .unwrap()
                .states_pruned
                > 0
        );
        let notes = db
            .list_notes(true)
            .unwrap()
            .into_iter()
            .map(|n| (n.id.clone(), db.get_note_full(&n.id).unwrap().unwrap()))
            .collect::<BTreeMap<_, _>>();
        db_export::export_md_vault(&f.out, &db).unwrap();
        let first = directory(&f.out);
        db_export::export_md_vault(&f.out, &db).unwrap();
        assert_eq!(directory(&f.out), first);
        let package = Package::load(&f.out).unwrap();
        let snapshot = Connection::open_in_memory().unwrap();
        package.materialize(&snapshot).unwrap();
        assert!(package.rows["revision_pins"]
            .iter()
            .all(|r| !text(r, 0).unwrap().contains(&pin)));
        let source_identity = ancestry::identity(&db.conn).unwrap();
        drop(db);
        let destination = f.dir.path().join("restored.rn");
        let ledger = f.dir.path().join("fresh-host-ledger");
        let operation = uuid::Uuid::new_v4().to_string();
        let outcome = restore_export(
            &f.out,
            &destination,
            &ledger,
            RestoreMode::FullRestore,
            &operation,
        )
        .unwrap();
        assert!(outcome.ledger_was_missing && outcome.requires_recovery_review);
        assert_eq!(outcome.identity.vault_uuid, source_identity.vault_uuid);
        assert_ne!(
            outcome.identity.storage_epoch,
            source_identity.storage_epoch
        );
        let restored = VaultDb::open_v3(&destination, &ledger).unwrap();
        for (id, note) in notes {
            assert_eq!(restored.get_note_full(&id).unwrap().unwrap(), note);
        }
        for spec in specs().unwrap() {
            if matches!(
                spec.name.as_str(),
                "revisions" | "revision_states" | "revision_parents"
            ) {
                let actual = read_rows(&restored.conn, &spec).unwrap();
                for row in &package.rows[&spec.name] {
                    assert!(
                        actual.contains(row),
                        "{} source identity/state must survive",
                        spec.name
                    );
                }
            } else if matches!(
                spec.name.as_str(),
                "attachments"
                    | "note_attachments"
                    | "versions"
                    | "session_links"
                    | "revision_pruning"
            ) {
                assert_eq!(
                    read_rows(&restored.conn, &spec).unwrap(),
                    package.rows[&spec.name],
                    "{}",
                    spec.name
                );
            }
        }
        assert_eq!(
            restored
                .get_attachment_data(&file.id)
                .unwrap()
                .unwrap()
                .0
                .note_ids,
            vec!["n", "other"]
        );
        let headers: i64 = restored
            .conn
            .query_row("SELECT COUNT(*) FROM revisions", [], |r| r.get(0))
            .unwrap();
        assert!(restored
            .accepted_at_token("n", head["accepted"]["token"].as_str().unwrap())
            .is_err());
        drop(restored);
        let retry = restore_export(
            &f.out,
            &destination,
            &ledger,
            RestoreMode::FullRestore,
            &operation,
        )
        .unwrap();
        assert_eq!(json!(retry), json!(outcome));
        let restored = VaultDb::open_v3(&destination, &ledger).unwrap();
        assert_eq!(
            restored
                .conn
                .query_row("SELECT COUNT(*) FROM revisions", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            headers
        );
        let export2 = f.dir.path().join("export-restored");
        db_export::export_md_vault(&export2, &restored).unwrap();
        let again = directory(&export2);
        db_export::export_md_vault(&export2, &restored).unwrap();
        assert_eq!(directory(&export2), again);
        drop(restored);
        std::fs::remove_file(ledger.join(format!("{}.json", source_identity.vault_uuid))).unwrap();
        assert!(restore_export(
            &f.out,
            &destination,
            &ledger,
            RestoreMode::FullRestore,
            &operation
        )
        .unwrap_err()
        .contains("ledger/epoch"));
        assert!(VaultDb::open_legacy_v2(&destination)
            .err()
            .unwrap()
            .contains("Unsupported vault schema version 3"));
    }
    #[test]
    fn independent_copy_translates_graph_authority_preserves_bytes_and_retries_one_operation() {
        let f = Fixture::new();
        let db = f.open();
        let base = write(&db, "n", "base\r\n", None);
        let head = write(&db, "n", "changed 雪\n", Some(&base));
        db.force_version("n", 0).unwrap();
        db_export::export_md_vault(&f.out, &db).unwrap();
        let original = ancestry::identity(&db.conn).unwrap();
        let old_rev = head["accepted"]["revision"]["revisionId"].as_str().unwrap();
        let path = f.dir.path().join("copy.rn");
        let ledger = f.dir.path().join("copy-ledger");
        let operation = uuid::Uuid::new_v4().to_string();
        let outcome = restore_export(
            &f.out,
            &path,
            &ledger,
            RestoreMode::IndependentCopy,
            &operation,
        )
        .unwrap();
        assert_ne!(outcome.identity.vault_uuid, original.vault_uuid);
        let copy = VaultDb::open_v3(&path, &ledger).unwrap();
        assert_eq!(
            copy.get_note_full("n").unwrap(),
            db.get_note_full("n").unwrap()
        );
        let exists: bool = copy
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM revisions WHERE revision_id=?1)",
                [old_rev],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!exists);
        let provenance: String = copy
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key=?1",
                [format!("portable_origin:v1:{operation}")],
                |r| r.get(0),
            )
            .unwrap();
        let provenance: Value = serde_json::from_str(&provenance).unwrap();
        assert_eq!(provenance["sourceVaultUuid"], original.vault_uuid);
        let new_id = provenance["graphTranslation"]["revision"][old_rev]
            .as_str()
            .unwrap();
        assert!(copy
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM revisions WHERE revision_id=?1)",
                [new_id],
                |r| r.get::<_, bool>(0)
            )
            .unwrap());
        assert!(copy
            .accepted_at_token("n", head["accepted"]["token"].as_str().unwrap())
            .is_err());
        drop(copy);
        let retry = restore_export(
            &f.out,
            &path,
            &ledger,
            RestoreMode::IndependentCopy,
            &operation,
        )
        .unwrap();
        assert_eq!(json!(retry), json!(outcome));
        let v2 = f.dir.path().join("empty-v2.rn");
        drop(VaultDb::open_legacy_v2(&v2).unwrap());
        restore_export(
            &f.out,
            &v2,
            &f.dir.path().join("empty-ledger"),
            RestoreMode::IndependentCopy,
            &uuid::Uuid::new_v4().to_string(),
        )
        .unwrap();
    }
    #[test]
    fn unknown_capabilities_and_damaged_bytes_fail_before_destination_ledger_or_source_lock_writes()
    {
        let f = Fixture::new();
        let db = f.open();
        write(&db, "n", "body", None);
        db_export::export_md_vault(&f.out, &db).unwrap();
        let index = f.out.join(db_export::INDEX);
        let original = std::fs::read(&index).unwrap();
        let baseline: Value = serde_json::from_slice(&original).unwrap();
        for mutation in ["format", "capability", "column", "extra"] {
            let mut forged = baseline.clone();
            match mutation {
                "format" => forged["format"] = json!(99),
                "capability" => forged["ancestry"]["capabilities"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!("future-capability")),
                "column" => forged["ancestry"]["tables"][0]["columns"][0] = json!("future-column"),
                _ => forged["futureRequiredCapability"] = json!(true),
            }
            std::fs::write(&index, serde_json::to_vec(&forged).unwrap()).unwrap();
            let destination = f.dir.path().join("must-not-exist.rn");
            let ledger = f.dir.path().join("must-not-exist-ledger");
            assert!(restore_export(
                &f.out,
                &destination,
                &ledger,
                RestoreMode::FullRestore,
                &uuid::Uuid::new_v4().to_string()
            )
            .is_err());
            assert!(!destination.exists() && !ledger.exists());
            assert!(super::super::db_import::import_md_vault(
                &f.out,
                &VaultDb::open_in_memory().unwrap()
            )
            .is_err());
        }
        std::fs::write(&index, original).unwrap();
        let package = Package::load(&f.out).unwrap();
        let sidecar = &package.manifest.sidecars[0];
        std::fs::write(f.out.join(&sidecar.file), b"corruption").unwrap();
        let destination = f.dir.path().join("still-absent.rn");
        let ledger = f.dir.path().join("still-absent-ledger");
        assert!(restore_export(
            &f.out,
            &destination,
            &ledger,
            RestoreMode::FullRestore,
            &uuid::Uuid::new_v4().to_string()
        )
        .is_err());
        assert!(!destination.exists() && !ledger.exists());
    }
    #[test]
    fn coherent_wal_export_keeps_one_generation_while_an_independent_writer_commits() {
        let f = Fixture::new();
        let db = f.open();
        write(&db, "n", "old exact\r\n", None);
        let writer = f.open();
        let read = db.conn.unchecked_transaction().unwrap();
        let manifest = capture(&db, &|file, bytes| {
            db_export::write(&f.out, file, bytes)?;
            Ok(())
        })
        .unwrap();
        // Snapshot stays live across a real independent WAL commit.
        writer.tag_atomic("n", &["new-tag".into()], &[]).unwrap();
        let again = capture(&db, &|file, bytes| db_export::write(&f.out, file, bytes)).unwrap();
        assert_eq!(
            manifest.request_hash().unwrap(),
            again.request_hash().unwrap()
        );
        read.commit().unwrap();
        let read = db.conn.unchecked_transaction().unwrap();
        let changed = capture(&db, &|file, bytes| db_export::write(&f.out, file, bytes)).unwrap();
        assert_ne!(
            manifest.request_hash().unwrap(),
            changed.request_hash().unwrap()
        );
        read.commit().unwrap();
    }
    #[test]
    fn explicit_reconciliation_adds_branches_without_head_promotion_and_rejects_immutable_collisions(
    ) {
        let f = Fixture::new();
        let db = f.open();
        let base = write(&db, "n", "B", None);
        db_export::export_md_vault(&f.out, &db).unwrap();
        let destination = f.dir.path().join("branch.rn");
        let ledger = f.dir.path().join("branch-ledger");
        restore_export(
            &f.out,
            &destination,
            &ledger,
            RestoreMode::FullRestore,
            &uuid::Uuid::new_v4().to_string(),
        )
        .unwrap();
        let target = VaultDb::open_v3(&destination, &ledger).unwrap();
        let before = target.accepted_note("n").unwrap().unwrap();
        // Simulate a known higher remote reservation; concurrent fresh-host
        // integer collisions otherwise MUST reject rather than renumber headers.
        {
            let mut ledger = ancestry::Ledger::acquire(
                &f.ledger,
                &ancestry::identity(&db.conn).unwrap().vault_uuid,
            )
            .unwrap();
            ledger.reserve_transaction(100).unwrap();
        }
        write(&db, "n", "remote branch", Some(&base));
        db_export::export_md_vault(&f.out, &db).unwrap();
        let operation = uuid::Uuid::new_v4().to_string();
        let receipt = reconcile_export(&f.out, &target, &operation).unwrap();
        assert!(receipt.headers_added > 0);
        assert_eq!(target.accepted_note("n").unwrap().unwrap(), before);
        assert_eq!(
            json!(reconcile_export(&f.out, &target, &operation).unwrap()),
            json!(receipt)
        );
        // Forge a self-consistent source state with a colliding immutable head ID.
        let mut package = Package::load(&f.out).unwrap();
        let source_head = package.rows["note_revision_heads"][0][2].clone();
        let source_id = match source_head {
            SqlValue::Text(v) => v,
            _ => panic!(),
        };
        let mut state = ancestry::payload(&db.conn, &source_id).unwrap();
        state.body = b"forged body".to_vec();
        for row in package.rows.get_mut("notes").unwrap() {
            if text(row, 0).unwrap() == "n" {
                row[2] = SqlValue::Text("forged body".into());
            }
        }
        for row in package.rows.get_mut("revision_states").unwrap() {
            if text(row, 0).unwrap() == source_id {
                row[1] = SqlValue::Blob(state.body.clone());
            }
        }
        for row in package.rows.get_mut("revisions").unwrap() {
            if text(row, 0).unwrap() == source_id {
                row[5] = SqlValue::Text(state.hash().unwrap());
            }
        }
        rewrite_package(&f.out, package);
        let archive_before = std::fs::read(f.out.join(db_export::INDEX)).unwrap();
        assert!(db_export::export_md_vault(&f.out, &db)
            .unwrap_err()
            .contains("Immutable"));
        assert_eq!(
            std::fs::read(f.out.join(db_export::INDEX)).unwrap(),
            archive_before
        );
        let count: i64 = target
            .conn
            .query_row("SELECT COUNT(*) FROM revisions", [], |r| r.get(0))
            .unwrap();
        assert!(
            reconcile_export(&f.out, &target, &uuid::Uuid::new_v4().to_string())
                .unwrap_err()
                .contains("collision")
        );
        assert_eq!(
            target
                .conn
                .query_row("SELECT COUNT(*) FROM revisions", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            count
        );
        assert_eq!(target.accepted_note("n").unwrap().unwrap(), before);
    }
    #[test]
    fn explicit_restore_ipc_is_scoped_idempotent_and_does_not_rebind_the_displayed_vault() {
        use crate::{commands, state::AppState, types::AppConfig};
        use tauri::Manager;
        use tauri_plugin_fs::FsExt;
        let f = Fixture::new();
        let db = f.open();
        write(&db, "n", "IPC body", None);
        db_export::export_md_vault(&f.out, &db).unwrap();
        let identity = ancestry::identity(&db.conn).unwrap();
        drop(db);
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_fs::init())
            .invoke_handler(tauri::generate_handler![
                commands::db_restore_ancestry_export,
                commands::db_reconcile_ancestry_export
            ])
            .manage(AppState::new(AppConfig::default()))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.fs_scope().allow_directory(f.dir.path(), true).unwrap();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        let request = |body: Value| {
            tauri::test::get_ipc_response(
                &webview,
                tauri::webview::InvokeRequest {
                    cmd: "db_restore_ancestry_export".into(),
                    callback: tauri::ipc::CallbackFn(0),
                    error: tauri::ipc::CallbackFn(1),
                    url: "http://tauri.localhost".parse().unwrap(),
                    body: tauri::ipc::InvokeBody::Json(body),
                    headers: Default::default(),
                    invoke_key: tauri::test::INVOKE_KEY.into(),
                },
            )
        };
        let body = json!({"sourceDir":f.out,"destination":f.dir.path().join("ipc.rn"),"ledgerRoot":f.dir.path().join("ipc-ledger"),"mode":"full_restore","operationId":uuid::Uuid::new_v4().to_string()});
        let first: Value = request(body.clone()).unwrap().deserialize().unwrap();
        assert_eq!(first["identity"]["vault_uuid"], identity.vault_uuid);
        assert_eq!(first["requires_recovery_review"], true);
        let retry: Value = request(body.clone()).unwrap().deserialize().unwrap();
        assert_eq!(first, retry);
        assert!(app.state::<AppState>().vault_db.lock().unwrap().is_none());
        let mut invalid = body.clone();
        invalid["mode"] = json!("guess-and-merge");
        assert!(request(invalid).is_err());
        let outside = tempfile::tempdir().unwrap();
        let mut denied = body;
        denied["destination"] = json!(outside.path().join("denied.rn"));
        assert!(request(denied).is_err());
        assert!(!outside.path().join("denied.rn").exists());
    }
}
