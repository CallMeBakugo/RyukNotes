use crate::types::{SearchMatch, SearchPage, SearchResult, SearchTarget, SearchTargetField};
use crate::vault::frontmatter;
use crate::vault::operations::helixnotes_dir;
use parking_lot::{Condvar, Mutex};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};
use tantivy::collector::{Count, TopDocs};
#[cfg(desktop)]
use tantivy::directory::MmapDirectory;
#[cfg(mobile)]
use tantivy::directory::RamDirectory;
use tantivy::query::{BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, Query, TermQuery};
use tantivy::schema::*;
use tantivy::tokenizer::{LowerCaser, TextAnalyzer, Token, TokenStream, Tokenizer};
use tantivy::{
    DocId, Index, IndexReader, IndexWriter, ReloadPolicy, SegmentReader, TantivyDocument, Term,
};
use walkdir::WalkDir;

/// Bumped whenever the index schema or tokenizer changes, so the on-disk index is
/// wiped and rebuilt once on the next vault open (the index is derived from the
/// notes, so this never loses data).
const INDEX_SCHEMA_VERSION: &str = "4-stored-body-streaming-cjk";
const INDEX_BATCH_DELAY: Duration = Duration::from_millis(300);
const MAX_PAGE_SIZE: usize = 100;
const SNIPPET_CHARS: usize = 240;
const SNIPPET_LEADING_CHARS: usize = 80;
const TARGET_CONTEXT_CHARS: usize = 32;

/// Per-vault search index dir in local app-data, kept out of the (possibly synced) vault.
#[cfg(desktop)]
fn vault_index_base(vault_path: &str) -> Option<std::path::PathBuf> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(vault_path.as_bytes());
    let key: String = hasher.finalize()[..8]
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect();
    dirs::data_local_dir().map(|d| d.join("helixnotes").join("search").join(key))
}

/// True for characters in the CJK / Japanese / Korean blocks, which are written
/// without spaces between words. These get uni/bi-gram tokenized so substring
/// search works; everything else keeps the default word-splitting behaviour.
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3400..=0x4DBF   // CJK Unified Ideographs Extension A
        | 0x4E00..=0x9FFF // CJK Unified Ideographs (Han)
        | 0x3040..=0x309F // Hiragana
        | 0x30A0..=0x30FF // Katakana
        | 0xAC00..=0xD7AF // Hangul Syllables
        | 0xF900..=0xFAFF // CJK Compatibility Ideographs
        | 0xFF00..=0xFFEF // Halfwidth and Fullwidth Forms
    )
}

/// Stream the same word / CJK uni-bigram tokens for indexing and highlighting.
/// Tokens stay in byte-offset order, without retaining a full document's tokens.
struct CjkTokenStream<'a> {
    text: &'a str,
    chars: std::iter::Peekable<std::str::CharIndices<'a>>,
    token: Token,
    bigram_end: Option<usize>,
}

impl TokenStream for CjkTokenStream<'_> {
    fn advance(&mut self) -> bool {
        self.token.position = self.token.position.wrapping_add(1);
        self.token.text.clear();
        if let Some(end) = self.bigram_end.take() {
            self.token.offset_to = end;
            self.token
                .text
                .push_str(&self.text[self.token.offset_from..end]);
            return true;
        }
        while let Some((start, c)) = self.chars.next() {
            if !c.is_alphanumeric() {
                continue;
            }
            let mut end = start + c.len_utf8();
            if is_cjk(c) {
                if let Some(&(offset, next)) = self.chars.peek() {
                    if next.is_alphanumeric() && is_cjk(next) {
                        self.bigram_end = Some(offset + next.len_utf8());
                    }
                }
            } else {
                while let Some(&(offset, next)) = self.chars.peek() {
                    if !next.is_alphanumeric() || is_cjk(next) {
                        break;
                    }
                    end = offset + next.len_utf8();
                    self.chars.next();
                }
            }
            self.token.offset_from = start;
            self.token.offset_to = end;
            self.token.text.push_str(&self.text[start..end]);
            return true;
        }
        false
    }

    fn token(&self) -> &Token {
        &self.token
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.token
    }
}

#[derive(Clone)]
struct CjkTokenizer;

impl Tokenizer for CjkTokenizer {
    type TokenStream<'a> = CjkTokenStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> CjkTokenStream<'a> {
        CjkTokenStream {
            text,
            chars: text.char_indices().peekable(),
            token: Token::default(),
            bigram_end: None,
        }
    }
}

struct SearchTerm {
    text: String,
    cjk: bool,
}

impl SearchTerm {
    fn matches(&self, token: &str) -> bool {
        if self.cjk {
            token == self.text
        } else {
            token.starts_with(&self.text) || within_one_edit(&self.text, token)
        }
    }
}

/// Distance at most one, including one adjacent transposition, as in Tantivy's
/// fuzzy query. Only the suffix after the first differing character is examined.
fn within_one_edit(left: &str, right: &str) -> bool {
    let mut left = left.chars();
    let mut right = right.chars();
    loop {
        match (left.next(), right.next()) {
            (Some(a), Some(b)) if a == b => continue,
            (Some(a), Some(b)) => {
                let left = left.as_str();
                let right = right.as_str();
                return left == right
                    || left.strip_prefix(b) == Some(right)
                    || right.strip_prefix(a) == Some(left)
                    || matches!(
                        (left.strip_prefix(b), right.strip_prefix(a)),
                        (Some(left), Some(right)) if left == right
                    );
            }
            (None, None) => return true,
            (Some(_), None) => return left.next().is_none(),
            (None, Some(_)) => return right.next().is_none(),
        }
    }
}

fn push_match(matches: &mut Vec<SearchMatch>, start: usize, end: usize) {
    if let Some(last) = matches.last_mut() {
        if start <= last.end {
            last.end = last.end.max(end);
            return;
        }
    }
    matches.push(SearchMatch { start, end });
}

/// Convert sorted, merged byte offsets in one forward pass; never count from
/// the beginning of a long note for every match.
fn utf16_matches(text: &str, mut matches: Vec<SearchMatch>, prefix: usize) -> Vec<SearchMatch> {
    let mut byte = 0;
    let mut utf16 = prefix;
    for range in &mut matches {
        utf16 += text[byte..range.start].encode_utf16().count();
        byte = range.start;
        range.start = utf16;
        utf16 += text[byte..range.end].encode_utf16().count();
        byte = range.end;
        range.end = utf16;
    }
    matches
}

fn title_byte_matches(
    title: &str,
    terms: &[SearchTerm],
    analyzer: &mut TextAnalyzer,
) -> Vec<SearchMatch> {
    let mut matches = Vec::new();
    let mut stream = analyzer.token_stream(title);
    while stream.advance() {
        let token = stream.token();
        if terms.iter().any(|term| term.matches(&token.text)) {
            push_match(&mut matches, token.offset_from, token.offset_to);
        }
    }
    matches
}

fn first_tag_match(
    tags: &str,
    terms: &[SearchTerm],
    analyzer: &mut TextAnalyzer,
) -> Option<SearchMatch> {
    let mut first: Option<SearchMatch> = None;
    let mut stream = analyzer.token_stream(tags);
    while stream.advance() {
        let token = stream.token();
        if first
            .as_ref()
            .is_some_and(|range| token.offset_from > range.end)
        {
            break;
        }
        if terms.iter().any(|term| term.matches(&token.text)) {
            if let Some(range) = &mut first {
                range.end = range.end.max(token.offset_to);
            } else {
                first = Some(SearchMatch {
                    start: token.offset_from,
                    end: token.offset_to,
                });
            }
        }
    }
    first
}

fn match_target(text: &str, field: SearchTargetField, start: usize, end: usize) -> SearchTarget {
    let end = text[start..end]
        .char_indices()
        .nth(SNIPPET_CHARS)
        .map_or(end, |(offset, _)| start + offset);
    let before = text[..start]
        .char_indices()
        .rev()
        .nth(TARGET_CONTEXT_CHARS - 1)
        .map_or(0, |(offset, _)| offset);
    let after = text[end..]
        .char_indices()
        .nth(TARGET_CONTEXT_CHARS)
        .map_or(text.len(), |(offset, _)| end + offset);
    let matched = &text[start..end];
    let utf16_start = text[..start].encode_utf16().count();
    SearchTarget {
        field,
        start: utf16_start,
        end: utf16_start + matched.encode_utf16().count(),
        text: matched.to_owned(),
        before: text[before..start].to_owned(),
        after: text[end..after].to_owned(),
    }
}

fn body_snippet(
    body: &str,
    terms: &[SearchTerm],
    analyzer: &mut TextAnalyzer,
) -> (String, Vec<SearchMatch>, Option<SearchTarget>) {
    let mut stream = analyzer.token_stream(body);
    let mut start = 0;
    let mut end = body
        .char_indices()
        .nth(SNIPPET_CHARS)
        .map_or(body.len(), |(offset, _)| offset);
    let mut found = false;
    let mut matches = Vec::new();
    while stream.advance() {
        let token = stream.token();
        if found && token.offset_from >= end {
            break;
        }
        if !terms.iter().any(|term| term.matches(&token.text)) {
            continue;
        }
        if !found {
            start = body[..token.offset_from]
                .char_indices()
                .rev()
                .nth(SNIPPET_LEADING_CHARS - 1)
                .map_or(0, |(offset, _)| offset);
            end = body[start..]
                .char_indices()
                .nth(SNIPPET_CHARS)
                .map_or(body.len(), |(offset, _)| start + offset);
            found = true;
        }
        push_match(
            &mut matches,
            token.offset_from - start,
            token.offset_to.min(end) - start,
        );
    }
    let target = matches.first().map(|range| {
        match_target(
            body,
            SearchTargetField::Body,
            start + range.start,
            start + range.end,
        )
    });
    let context = &body[start..end];
    let prefix = usize::from(start > 0);
    let matches = utf16_matches(context, matches, prefix);
    let mut snippet = String::with_capacity(context.len() + 6);
    if start > 0 {
        snippet.push('…');
    }
    snippet.push_str(context);
    if end < body.len() {
        snippet.push('…');
    }
    (snippet, matches, target)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileStamp {
    seconds: i64,
    nanos: i64,
    size: u64,
}

impl FileStamp {
    fn read(path: &Path) -> std::io::Result<Self> {
        let metadata = fs::metadata(path)?;
        let modified = metadata.modified()?;
        let (duration, sign) = match modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => (duration, 1),
            Err(error) => (error.duration(), -1),
        };
        Ok(Self {
            seconds: duration.as_secs() as i64 * sign,
            nanos: i64::from(duration.subsec_nanos()) * sign,
            size: metadata.len(),
        })
    }
}

#[derive(Default)]
struct IndexQueue {
    pending: HashSet<String>,
    refresh: Option<String>,
    running: bool,
    initial_refresh_pending: bool,
}

#[derive(Default)]
pub(crate) struct SearchIndexShutdown {
    finished: Mutex<bool>,
    ready: Condvar,
    #[cfg(test)]
    wait_started: std::sync::atomic::AtomicBool,
}

impl SearchIndexShutdown {
    pub(crate) fn is_finished(&self) -> bool {
        self.finished.try_lock().is_some_and(|finished| *finished)
    }

    pub(crate) fn wait(&self) {
        #[cfg(test)]
        self.wait_started
            .store(true, std::sync::atomic::Ordering::Release);
        let mut finished = self.finished.lock();
        while !*finished {
            self.ready.wait(&mut finished);
        }
    }
}

pub struct SearchIndex {
    index: Index,
    reader: IndexReader,
    writer: Mutex<Option<IndexWriter>>,
    queue: Mutex<IndexQueue>,
    queue_ready: Condvar,
    shutdown: Arc<SearchIndexShutdown>,
    #[allow(dead_code)]
    schema: Schema,
    path_field: Field,
    title_field: Field,
    body_field: Field,
    tags_field: Field,
    modified_seconds_field: Field,
    modified_nanos_field: Field,
    size_field: Field,
}

impl Drop for SearchIndex {
    fn drop(&mut self) {
        let mut finished = self.shutdown.finished.lock();
        // Arc's last strong reference disappears before Drop runs. Do not let
        // a reopening vault create a writer until Tantivy releases its lock.
        drop(self.writer.get_mut().take());
        *finished = true;
        self.shutdown.ready.notify_all();
    }
}

impl SearchIndex {
    pub(crate) fn shutdown_signal(&self) -> Arc<SearchIndexShutdown> {
        self.shutdown.clone()
    }

    pub fn new(vault_path: &str) -> Result<Self, String> {
        let mut schema_builder = Schema::builder();
        let path_field = schema_builder.add_text_field("path", STRING | STORED);
        // CJK-aware tokenizer for the indexed text fields (keeps Latin/English
        // behaviour identical; adds substring matching for Chinese/Japanese/Korean).
        let cjk_indexing = TextFieldIndexing::default()
            .set_tokenizer("cjk")
            .set_index_option(IndexRecordOption::WithFreqsAndPositions);
        let cjk_text = TextOptions::default().set_indexing_options(cjk_indexing.clone());
        let cjk_text_stored = cjk_text.clone().set_stored();
        let title_field = schema_builder.add_text_field("title", cjk_text_stored.clone());
        let body_field = schema_builder.add_text_field("body", cjk_text_stored.clone());
        let tags_field = schema_builder.add_text_field("tags", cjk_text_stored);
        let modified_seconds_field = schema_builder.add_i64_field("modified_seconds", STORED);
        let modified_nanos_field = schema_builder.add_i64_field("modified_nanos", STORED);
        let size_field = schema_builder.add_u64_field("size", STORED);
        let schema = schema_builder.build();

        // Mobile: use in-memory index (flock is unreliable on the sandboxed/FUSE filesystem)
        // Desktop: use mmap directory for persistent index on disk
        #[cfg(mobile)]
        let index = {
            let dir = RamDirectory::create();
            Index::open_or_create(dir, schema.clone()).map_err(|e| e.to_string())?
        };
        #[cfg(desktop)]
        let index = {
            let base = vault_index_base(vault_path)
                .unwrap_or_else(|| helixnotes_dir(vault_path).join("search_index"));
            let index_dir = base.join("index");
            let version_path = base.join("version");

            // Remove the old in-vault index so it stops syncing.
            let hn = helixnotes_dir(vault_path);
            let _ = fs::remove_dir_all(hn.join("search_index"));
            let _ = fs::remove_file(hn.join("search_index.version"));

            // One-time wipe when the schema/tokenizer changes; refresh repopulates.
            let version_ok = fs::read_to_string(&version_path)
                .map(|v| v.trim() == INDEX_SCHEMA_VERSION)
                .unwrap_or(false);
            if !version_ok {
                let _ = fs::remove_dir_all(&index_dir);
            }
            fs::create_dir_all(&index_dir).map_err(|e| e.to_string())?;
            let dir = MmapDirectory::open(&index_dir).map_err(|e| e.to_string())?;
            let idx = match Index::open_or_create(dir, schema.clone()) {
                Ok(idx) => idx,
                Err(_) => {
                    // Schema mismatch (older index) or corruption: wipe and recreate.
                    let _ = fs::remove_dir_all(&index_dir);
                    fs::create_dir_all(&index_dir).map_err(|e| e.to_string())?;
                    let dir = MmapDirectory::open(&index_dir).map_err(|e| e.to_string())?;
                    Index::open_or_create(dir, schema.clone()).map_err(|e| e.to_string())?
                }
            };
            let _ = fs::write(&version_path, INDEX_SCHEMA_VERSION);
            idx
        };

        // Register the CJK-aware tokenizer (in-memory; must be done on every open,
        // before the writer is created, so both indexing and querying use it).
        index.tokenizers().register(
            "cjk",
            TextAnalyzer::builder(CjkTokenizer)
                .filter(LowerCaser)
                .build(),
        );

        #[cfg(mobile)]
        let heap_size = 15_000_000;
        #[cfg(desktop)]
        let heap_size = 50_000_000;

        let writer = index.writer(heap_size).map_err(|e| e.to_string())?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(|e: tantivy::TantivyError| e.to_string())?;
        let initial_refresh_pending = index.load_metas().map_err(|e| e.to_string())?.opstamp == 0;

        Ok(Self {
            index,
            reader,
            writer: Mutex::new(Some(writer)),
            queue: Mutex::new(IndexQueue {
                initial_refresh_pending,
                ..IndexQueue::default()
            }),
            queue_ready: Condvar::new(),
            shutdown: Arc::new(SearchIndexShutdown::default()),
            schema,
            path_field,
            title_field,
            body_field,
            tags_field,
            modified_seconds_field,
            modified_nanos_field,
            size_field,
        })
    }

    pub fn rebuild(&self, vault_path: &str) -> Result<(), String> {
        let mut writer_guard = self.writer.lock();
        let writer = writer_guard.as_mut().ok_or("Writer not available")?;
        writer.delete_all_documents().map_err(|e| e.to_string())?;
        for entry in Self::vault_entries(vault_path) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    let _ = writer.rollback();
                    return Err(error.to_string());
                }
            };
            if !Self::is_note(&entry) {
                continue;
            }
            let path = entry.path().to_string_lossy();
            match self.note_document(&path) {
                Ok(Some(doc)) => {
                    if let Err(error) = writer.add_document(doc) {
                        let _ = writer.rollback();
                        return Err(error.to_string());
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = writer.rollback();
                    return Err(error);
                }
            }
        }
        self.commit(writer)
    }

    fn vault_entries(vault_path: &str) -> impl Iterator<Item = walkdir::Result<walkdir::DirEntry>> {
        let root = Path::new(vault_path).to_path_buf();
        WalkDir::new(vault_path)
            .into_iter()
            .filter_entry(move |entry| {
                entry.path() == root
                    || !entry
                        .file_name()
                        .to_str()
                        .map(|name| name.starts_with('.'))
                        .unwrap_or(false)
            })
    }

    fn is_note(entry: &walkdir::DirEntry) -> bool {
        entry.file_type().is_file()
            && entry.path().extension().and_then(|ext| ext.to_str()) == Some("md")
    }

    /// Reconcile persistent entries on the same worker as note updates. Enqueuing
    /// never traverses the vault or waits for the writer; the cached reader is live.
    pub fn queue_vault_refresh(self: &Arc<Self>, vault_path: &str) -> Result<(), String> {
        let mut queue = self.queue.lock();
        queue.refresh = Some(vault_path.to_owned());
        self.start_worker(&mut queue)
    }

    /// Schedule a fixed-window batch, rather than delaying every time a note changes.
    /// The worker owns the index only until its queue is empty.
    pub fn queue_index_note(self: &Arc<Self>, path: &str) -> Result<(), String> {
        let mut queue = self.queue.lock();
        if !queue.pending.contains(path) {
            queue.pending.insert(path.to_owned());
        }
        self.start_worker(&mut queue)
    }

    fn start_worker(self: &Arc<Self>, queue: &mut IndexQueue) -> Result<(), String> {
        if queue.running {
            return Ok(());
        }
        queue.running = true;
        let search = Arc::clone(self);
        if let Err(error) = std::thread::Builder::new()
            .name("search-index".into())
            .spawn(move || search.drain_index_queue())
        {
            queue.running = false;
            self.queue_ready.notify_all();
            return Err(format!("Could not start search index worker: {error}"));
        }
        Ok(())
    }

    fn drain_index_queue(&self) {
        let mut batch = HashSet::new();
        loop {
            std::thread::sleep(INDEX_BATCH_DELAY);
            let refresh = {
                let mut queue = self.queue.lock();
                std::mem::swap(&mut batch, &mut queue.pending);
                queue.refresh.take()
            };
            if let Some(vault_path) = refresh {
                if let Err(error) = self.refresh_vault(&vault_path) {
                    log::error!("Could not reconcile search index: {error}");
                }
                self.finish_initial_refresh();
            }
            if let Err(error) = self.index_batch(&batch) {
                log::error!("Could not commit search index batch: {error}");
            }
            batch.clear();

            // Clearing `running` and checking for more work share the enqueue lock,
            // so an edit arriving as the worker exits always starts another worker.
            let mut queue = self.queue.lock();
            if queue.pending.is_empty() && queue.refresh.is_none() {
                queue.running = false;
                self.queue_ready.notify_all();
                return;
            }
        }
    }

    fn finish_initial_refresh(&self) {
        let mut queue = self.queue.lock();
        queue.initial_refresh_pending = false;
        self.queue_ready.notify_all();
    }

    fn indexed_stamps(&self) -> Result<HashMap<String, FileStamp>, String> {
        let searcher = self.reader.searcher();
        let mut stamps = HashMap::new();
        for (segment_ord, segment) in searcher.segment_readers().iter().enumerate() {
            for doc_id in segment.doc_ids_alive() {
                let doc: TantivyDocument = searcher
                    .doc(tantivy::DocAddress::new(segment_ord as u32, doc_id))
                    .map_err(|e| e.to_string())?;
                if let Some(path) = doc.get_first(self.path_field).and_then(|v| v.as_str()) {
                    stamps.insert(
                        path.to_owned(),
                        FileStamp {
                            seconds: doc
                                .get_first(self.modified_seconds_field)
                                .and_then(|v| v.as_i64())
                                .unwrap_or_default(),
                            nanos: doc
                                .get_first(self.modified_nanos_field)
                                .and_then(|v| v.as_i64())
                                .unwrap_or_default(),
                            size: doc
                                .get_first(self.size_field)
                                .and_then(|v| v.as_u64())
                                .unwrap_or_default(),
                        },
                    );
                }
            }
        }
        Ok(stamps)
    }

    fn refresh_vault(&self, vault_path: &str) -> Result<(), String> {
        // Serialize reads and mutations with queued edits, removals and rebuilds.
        // Never collect note bodies before taking this lock: a stale snapshot
        // could otherwise replace an already-committed save.
        let mut writer_guard = self.writer.lock();
        let writer = writer_guard.as_mut().ok_or("Writer not available")?;
        self.reader.reload().map_err(|e| e.to_string())?;
        let mut indexed = self.indexed_stamps()?;
        let mut changed = false;
        let mut complete = true;
        for entry in Self::vault_entries(vault_path) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    log::error!("Could not inspect vault for search: {error}");
                    complete = false;
                    continue;
                }
            };
            if !Self::is_note(&entry) {
                continue;
            }
            let path = entry.path().to_string_lossy();
            let previous = indexed.remove(path.as_ref());
            match FileStamp::read(entry.path()) {
                Ok(stamp) if previous == Some(stamp) => continue,
                Ok(_) => {}
                Err(error) => {
                    log::error!("Could not inspect note {path}: {error}");
                    continue;
                }
            }
            match self.note_document(&path) {
                Ok(doc) => {
                    self.replace_document(writer, &path, doc)?;
                    changed = true;
                }
                Err(error) => log::error!("Could not index note {path}: {error}"),
            }
        }
        if complete {
            for path in indexed.keys() {
                // Inaccessible directories and transient I/O failures must not
                // turn a partial traversal into deletion of valid entries.
                let removed = match fs::metadata(path) {
                    Ok(metadata) => !metadata.is_file(),
                    Err(error) => error.kind() == std::io::ErrorKind::NotFound,
                };
                if removed {
                    writer.delete_term(Term::from_field_text(self.path_field, path));
                    changed = true;
                }
            }
        }
        if changed {
            self.commit(writer)?;
        }
        Ok(())
    }

    fn index_batch(&self, paths: &HashSet<String>) -> Result<(), String> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut writer_guard = self.writer.lock();
        let writer = writer_guard.as_mut().ok_or("Writer not available")?;
        let mut changed = false;
        for path in paths {
            match self.note_document(path) {
                Ok(doc) => {
                    self.replace_document(writer, path, doc)?;
                    changed = true;
                }
                Err(error) => log::error!("Could not index note {path}: {error}"),
            }
        }
        if changed {
            self.commit(writer)?;
        }
        Ok(())
    }

    fn note_document(&self, path: &str) -> Result<Option<TantivyDocument>, String> {
        let p = Path::new(path);
        let stamp = match FileStamp::read(p) {
            Ok(stamp) => stamp,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let raw = match fs::read_to_string(p) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        if FileStamp::read(p).map_err(|e| e.to_string())? != stamp {
            return Err("Note changed while being indexed".to_string());
        }
        let filename = p.file_name().unwrap_or_default().to_string_lossy();
        let (meta, content) = frontmatter::parse_note(&raw, &filename);
        let mut doc = TantivyDocument::new();
        doc.add_text(self.path_field, path);
        doc.add_text(self.title_field, &meta.title);
        doc.add_text(self.body_field, &content);
        doc.add_text(self.tags_field, meta.tags.join(" "));
        doc.add_i64(self.modified_seconds_field, stamp.seconds);
        doc.add_i64(self.modified_nanos_field, stamp.nanos);
        doc.add_u64(self.size_field, stamp.size);
        Ok(Some(doc))
    }

    fn replace_document(
        &self,
        writer: &mut IndexWriter,
        path: &str,
        doc: Option<TantivyDocument>,
    ) -> Result<(), String> {
        writer.delete_term(Term::from_field_text(self.path_field, path));
        if let Some(doc) = doc {
            if let Err(error) = writer.add_document(doc) {
                let _ = writer.rollback();
                return Err(error.to_string());
            }
        }
        Ok(())
    }

    fn commit(&self, writer: &mut IndexWriter) -> Result<(), String> {
        if let Err(error) = writer.commit() {
            let _ = writer.rollback();
            return Err(error.to_string());
        }
        self.reader.reload().map_err(|e| e.to_string())?;
        self.finish_initial_refresh();
        Ok(())
    }

    pub fn remove_note(&self, path: &str) -> Result<(), String> {
        let mut writer_guard = self.writer.lock();
        let writer = writer_guard.as_mut().ok_or("Writer not available")?;
        writer.delete_term(Term::from_field_text(self.path_field, path));
        self.commit(writer)
    }

    pub fn search(
        &self,
        query_str: &str,
        limit: usize,
        offset: usize,
    ) -> Result<SearchPage, String> {
        let mut analyzer = self
            .index
            .tokenizers()
            .get("cjk")
            .ok_or("cjk tokenizer not registered")?;
        let terms: Vec<SearchTerm> = {
            let mut out = Vec::new();
            let mut stream = analyzer.token_stream(query_str);
            while stream.advance() {
                out.push(stream.token().text.clone());
            }
            out.sort_unstable();
            out.dedup();
            out.into_iter()
                .map(|text| SearchTerm {
                    cjk: text.chars().any(is_cjk),
                    text,
                })
                .collect()
        };
        // A zero-clause BooleanQuery must never become a browse-all query.
        if terms.is_empty() {
            return Ok(SearchPage::default());
        }
        {
            let mut queue = self.queue.lock();
            // A cold/schema-migrated index has no useful cached answer. Search
            // runs off-thread and waits for its first queued refresh, not for
            // later maintenance of an already-committed persistent index.
            while queue.initial_refresh_pending && queue.running {
                self.queue_ready.wait(&mut queue);
            }
        }
        let searcher = self.reader.searcher();
        // Native BM25 exact matches receive a 4x boost; title/tag/body weights
        // are 4/2/1. Prefix matches add 1x and one-edit typos only 0.5x.
        // Each distinct term is required, but can occur in any indexed field.
        let fields = [
            (self.title_field, 4.0),
            (self.tags_field, 2.0),
            (self.body_field, 1.0),
        ];
        let term_queries: Vec<(Occur, Box<dyn Query>)> = terms
            .iter()
            .map(|term| {
                let mut field_queries: Vec<(Occur, Box<dyn Query>)> = Vec::new();
                for &(field, field_boost) in &fields {
                    let indexed_term = Term::from_field_text(field, &term.text);
                    field_queries.push((
                        Occur::Should,
                        Box::new(BoostQuery::new(
                            Box::new(TermQuery::new(
                                indexed_term.clone(),
                                IndexRecordOption::WithFreqs,
                            )),
                            field_boost * 4.0,
                        )),
                    ));
                    if !term.cjk {
                        // A zero-edit prefix automaton has no expansion cap,
                        // unlike PhrasePrefixQuery, so short prefixes keep all hits.
                        field_queries.push((
                            Occur::Should,
                            Box::new(BoostQuery::new(
                                Box::new(FuzzyTermQuery::new_prefix(indexed_term.clone(), 0, true)),
                                field_boost,
                            )),
                        ));
                        field_queries.push((
                            Occur::Should,
                            Box::new(BoostQuery::new(
                                Box::new(FuzzyTermQuery::new(indexed_term, 1, true)),
                                field_boost * 0.5,
                            )),
                        ));
                    }
                }
                let combined: Box<dyn Query> = Box::new(BooleanQuery::new(field_queries));
                (Occur::Must, combined)
            })
            .collect();
        let query = BooleanQuery::new(term_queries);
        let doc_count = usize::try_from(searcher.num_docs()).unwrap_or(usize::MAX);
        if offset >= doc_count {
            return Ok(SearchPage {
                results: Vec::new(),
                total: searcher.search(&query, &Count).map_err(|e| e.to_string())?,
            });
        }
        let limit = limit.clamp(1, MAX_PAGE_SIZE).min(doc_count - offset);
        let segment_order: HashMap<_, _> = searcher
            .segment_readers()
            .iter()
            .enumerate()
            .map(|(ordinal, segment)| (segment.segment_id(), ordinal))
            .collect();
        // TopDocs alone does not promise stable ties. Include the complete
        // document address without changing the reported relevance score.
        let top_docs = TopDocs::with_limit(limit).and_offset(offset).tweak_score(
            move |segment: &SegmentReader| {
                let ordinal = segment_order[&segment.segment_id()];
                move |doc: DocId, score: f32| (score, Reverse(ordinal), Reverse(doc))
            },
        );
        let (top_docs, total) = searcher
            .search(&query, &(top_docs, Count))
            .map_err(|e| e.to_string())?;
        let mut results = Vec::with_capacity(top_docs.len());
        for ((score, _, _), doc_address) in top_docs {
            let doc: TantivyDocument = searcher.doc(doc_address).map_err(|e| e.to_string())?;
            let path = doc
                .get_first(self.path_field)
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let title = doc
                .get_first(self.title_field)
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let body = doc
                .get_first(self.body_field)
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let title_ranges = title_byte_matches(title, &terms, &mut analyzer);
            let (snippet, snippet_matches, body_target) = body_snippet(body, &terms, &mut analyzer);
            let match_target = body_target
                .or_else(|| {
                    title_ranges.first().map(|range| {
                        match_target(title, SearchTargetField::Title, range.start, range.end)
                    })
                })
                .or_else(|| {
                    let tags = doc.get_first(self.tags_field).and_then(|v| v.as_str())?;
                    first_tag_match(tags, &terms, &mut analyzer).map(|range| {
                        match_target(tags, SearchTargetField::Tags, range.start, range.end)
                    })
                });
            let title_matches = utf16_matches(title, title_ranges, 0);
            results.push(SearchResult {
                path: path.to_owned(),
                title: title.to_owned(),
                snippet,
                score,
                title_matches,
                snippet_matches,
                match_target,
            });
        }
        Ok(SearchPage { results, total })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TestVault(PathBuf);

    impl TestVault {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("helixnotes-search-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn index(&self) -> Arc<SearchIndex> {
            Arc::new(SearchIndex::new(self.0.to_str().unwrap()).unwrap())
        }
    }

    impl Drop for TestVault {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
            #[cfg(desktop)]
            if let Some(base) = vault_index_base(self.0.to_str().unwrap()) {
                let _ = fs::remove_dir_all(base);
            }
        }
    }

    fn result_paths(index: &SearchIndex, query: &str) -> Vec<String> {
        let mut paths: Vec<_> = index
            .search(query, 10, 0)
            .unwrap()
            .results
            .into_iter()
            .map(|result| result.path)
            .collect();
        paths.sort();
        paths
    }

    fn write_search_note(vault: &TestVault, name: &str, title: &str, body: &str) -> String {
        let path = vault.0.join(name);
        fs::write(
            &path,
            format!("---\ntitle: '{}'\n---\n{}", title.replace('\'', "''"), body),
        )
        .unwrap();
        path.to_str().unwrap().to_owned()
    }

    fn highlighted(text: &str, matches: &[SearchMatch]) -> Vec<String> {
        let utf16: Vec<_> = text.encode_utf16().collect();
        let mut previous_end = 0;
        matches
            .iter()
            .map(|range| {
                assert!(range.start >= previous_end);
                assert!(range.start < range.end);
                previous_end = range.end;
                String::from_utf16(&utf16[range.start..range.end]).unwrap()
            })
            .collect()
    }

    fn assert_target<'a>(
        result: &'a SearchResult,
        source: &str,
        field: SearchTargetField,
        matched: &str,
    ) -> &'a SearchTarget {
        let target = result.match_target.as_ref().unwrap();
        assert_eq!(target.field, field);
        assert_eq!(target.text, matched);
        assert!(target.text.chars().count() <= SNIPPET_CHARS);
        let utf16: Vec<_> = source.encode_utf16().collect();
        assert_eq!(
            String::from_utf16(&utf16[target.start..target.end]).unwrap(),
            matched
        );
        let before = String::from_utf16(&utf16[..target.start]).unwrap();
        let after = String::from_utf16(&utf16[target.end..]).unwrap();
        assert!(before.ends_with(&target.before));
        assert!(after.starts_with(&target.after));
        assert_eq!(
            target.before.chars().count(),
            before.chars().count().min(TARGET_CONTEXT_CHARS)
        );
        assert_eq!(
            target.after.chars().count(),
            after.chars().count().min(TARGET_CONTEXT_CHARS)
        );
        target
    }

    #[test]
    fn exact_and_title_matches_outrank_prefix_and_typo_body_matches() {
        let vault = TestVault::new();
        let title = write_search_note(&vault, "title.md", "Watermelon", "summer harvest");
        let body = write_search_note(&vault, "body.md", "Kitchen", "watermelon");
        let fuzzy_title =
            write_search_note(&vault, "fuzzy-title.md", "Watermelom", "summer harvest");
        let fuzzy_body = write_search_note(&vault, "fuzzy-body.md", "Workshop", "watermelom");
        let prefix = write_search_note(&vault, "prefix.md", "Garden", "watermelons");
        let index = vault.index();
        index.rebuild(vault.0.to_str().unwrap()).unwrap();
        let page = index.search("watermelon", 20, 0).unwrap();
        assert_eq!(page.total, 5);
        let rank = |path: &str| {
            page.results
                .iter()
                .position(|result| result.path == path)
                .unwrap()
        };
        assert!(rank(&title) < rank(&body));
        assert!(rank(&body) < rank(&fuzzy_title));
        assert!(rank(&body) < rank(&fuzzy_body));
        assert!(rank(&body) < rank(&prefix));
        assert!(rank(&fuzzy_title) < rank(&fuzzy_body));
        assert_eq!(
            highlighted(&page.results[0].title, &page.results[0].title_matches),
            vec!["Watermelon"]
        );
        assert!(page
            .results
            .windows(2)
            .all(|pair| pair[0].score >= pair[1].score));
    }

    #[test]
    fn literal_terms_keep_prefix_typo_cjk_and_cross_field_and_matching() {
        let vault = TestVault::new();
        let path = write_search_note(
            &vault,
            "archive.md",
            "Archive",
            "strawberry 東京大学 カタカナ 한국어 café",
        );
        let index = vault.index();
        index.rebuild(vault.0.to_str().unwrap()).unwrap();
        for query in [
            "str",
            "strawbery",
            "strawberrry",
            "strawberxy",
            "strawberyr",
            "cafe",
            "東京",
            "京大",
            "タカ",
            "한국",
            "archive strawberyr 東京",
        ] {
            let page = index.search(query, 20, 0).unwrap();
            assert_eq!(page.total, 1, "{query}");
            assert_eq!(page.results[0].path, path, "{query}");
            assert!(
                !highlighted(&page.results[0].snippet, &page.results[0].snippet_matches).is_empty(),
                "{query}"
            );
        }
        for query in [
            "",
            "   ",
            "!!! * & ()",
            "，。！？",
            "東京 missing",
            "strawbrreyy",
        ] {
            let page = index.search(query, 20, 0).unwrap();
            assert_eq!(page.total, 0, "{query}");
            assert!(page.results.is_empty(), "{query}");
        }
    }

    #[test]
    fn short_prefix_pages_have_exact_totals_and_stable_ties_without_expansion_loss() {
        let vault = TestVault::new();
        for i in 0..65 {
            write_search_note(
                &vault,
                &format!("note-{i:03}.md"),
                "Archive",
                &format!("common{i:03}"),
            );
        }
        let index = vault.index();
        index.rebuild(vault.0.to_str().unwrap()).unwrap();
        let all = index.search("com", usize::MAX, 0).unwrap();
        assert_eq!(all.total, 65);
        let expected: Vec<_> = all
            .results
            .iter()
            .map(|result| result.path.clone())
            .collect();
        assert_eq!(expected.len(), 65);
        let mut paged = Vec::new();
        for offset in [0, 20, 40, 60] {
            let page = index.search("com", 20, offset).unwrap();
            assert_eq!(page.total, 65);
            assert_eq!(page.results.len(), (65 - offset).min(20));
            paged.extend(page.results.into_iter().map(|result| result.path));
        }
        assert_eq!(paged, expected);
        assert_eq!(paged.iter().collect::<HashSet<_>>().len(), 65);
        assert_eq!(
            index
                .search("com", 20, 20)
                .unwrap()
                .results
                .into_iter()
                .map(|result| result.path)
                .collect::<Vec<_>>(),
            expected[20..40]
        );
        let beyond = index.search("com", usize::MAX, usize::MAX).unwrap();
        assert_eq!(beyond.total, 65);
        assert!(beyond.results.is_empty());
        let minimum = index.search("com", 0, 0).unwrap();
        assert_eq!(minimum.total, 65);
        assert_eq!(minimum.results[0].path, expected[0]);
        assert_eq!(minimum.results.len(), 1);
    }

    #[test]
    fn snippets_find_late_matches_and_return_literal_unicode_utf16_ranges() {
        let vault = TestVault::new();
        let body = format!(
            "{}🦊 <script>alert('literal')</script> Café, strawberry 東京大学 {}",
            "quiet preamble. ".repeat(100),
            "quiet ending. ".repeat(100),
        );
        let path = write_search_note(&vault, "late.md", "🦊 Café 東京", &body);
        let index = vault.index();
        index.rebuild(vault.0.to_str().unwrap()).unwrap();
        let result = index
            .search("cafe strawberyr 東京", 20, 0)
            .unwrap()
            .results
            .remove(0);
        assert_eq!(result.path, path);
        assert!(result.snippet.starts_with('…'));
        assert!(result.snippet.ends_with('…'));
        assert!(result.snippet.chars().count() <= SNIPPET_CHARS + 2);
        assert!(result.snippet.contains("<script>alert('literal')</script>"));
        assert_eq!(
            highlighted(&result.snippet, &result.snippet_matches),
            vec!["Café", "strawberry", "東京"]
        );
        assert_eq!(
            highlighted(&result.title, &result.title_matches),
            vec!["Café", "東京"]
        );
        assert_eq!(result.title_matches[0], SearchMatch { start: 3, end: 7 });
        let target = assert_target(&result, &body, SearchTargetField::Body, "Café");
        assert_eq!(
            target.start,
            body[..body.find("Café").unwrap()].encode_utf16().count()
        );
        // Prefixes highlight the actual matched indexed token, not an invented
        // exact substring; overlapping CJK uni/bigrams become one range.
        let prefix = index.search("straw", 20, 0).unwrap().results.remove(0);
        assert_eq!(
            highlighted(&prefix.snippet, &prefix.snippet_matches),
            vec!["strawberry"]
        );
        assert_target(&prefix, &body, SearchTargetField::Body, "strawberry");
        let fuzzy = index.search("strawberyr", 20, 0).unwrap().results.remove(0);
        assert_target(&fuzzy, &body, SearchTargetField::Body, "strawberry");
        let cjk = index.search("東京大学", 20, 0).unwrap().results.remove(0);
        assert_eq!(
            highlighted(&cjk.snippet, &cjk.snippet_matches),
            vec!["東京大学"]
        );
        assert_target(&cjk, &body, SearchTargetField::Body, "東京大学");
    }

    #[test]
    fn snippets_cover_case_expansion_clipped_tokens_and_title_or_tag_only_hits() {
        let vault = TestVault::new();
        write_search_note(&vault, "unicode.md", "🦊 İSTANBUL", "🦊 İSTANBUL");
        let long_word = format!("prefix{}", "x".repeat(1000));
        write_search_note(&vault, "long.md", "Long token", &long_word);
        let tagged = vault.0.join("tagged.md");
        fs::write(
            &tagged,
            "---\ntitle: Heading\ntags: [watermelon]\n---\nPlain body without the requested token.",
        )
        .unwrap();
        let index = vault.index();
        index.rebuild(vault.0.to_str().unwrap()).unwrap();
        let unicode = index.search("İSTANBUL", 20, 0).unwrap().results.remove(0);
        assert_eq!(
            highlighted(&unicode.snippet, &unicode.snippet_matches),
            vec!["İSTANBUL"]
        );
        assert_eq!(
            unicode.title_matches,
            vec![SearchMatch { start: 3, end: 11 }]
        );
        let target = assert_target(&unicode, "🦊 İSTANBUL", SearchTargetField::Body, "İSTANBUL");
        assert_eq!((target.start, target.end), (3, 11));
        let long = index.search("prefix", 20, 0).unwrap().results.remove(0);
        assert_eq!(long.snippet.chars().count(), SNIPPET_CHARS + 1);
        assert_eq!(
            highlighted(&long.snippet, &long.snippet_matches),
            vec![long_word[..SNIPPET_CHARS].to_owned()]
        );
        assert_target(
            &long,
            &long_word,
            SearchTargetField::Body,
            &long_word[..SNIPPET_CHARS],
        );
        for query in ["heading", "watermelon"] {
            let result = index.search(query, 20, 0).unwrap().results.remove(0);
            assert_eq!(result.path, tagged.to_str().unwrap());
            assert_eq!(result.snippet, "Plain body without the requested token.");
            assert!(result.snippet_matches.is_empty());
            let (field, text) = if query == "heading" {
                (SearchTargetField::Title, "Heading")
            } else {
                (SearchTargetField::Tags, "watermelon")
            };
            assert_target(&result, text, field, text);
        }
    }

    #[test]
    fn targets_prefer_first_body_then_title_then_stored_tags() {
        let vault = TestVault::new();
        let title = "🦊 Watermelon Strawberry";
        let body = "quiet 🦊 STRAWBERRY strawberry";
        let tags = "watermelon pineapple strawberry 東京大学";
        fs::write(
            vault.0.join("priority.md"),
            format!(
                "---\ntitle: '{title}'\ntags: [watermelon, pineapple, strawberry, 東京大学]\n---\n{body}"
            ),
        )
        .unwrap();
        let index = vault.index();
        index.rebuild(vault.0.to_str().unwrap()).unwrap();
        let body_hit = index.search("straw", 20, 0).unwrap().results.remove(0);
        let target = assert_target(&body_hit, body, SearchTargetField::Body, "STRAWBERRY");
        assert_eq!((target.start, target.end), (9, 19));
        let title_hit = index.search("watermelno", 20, 0).unwrap().results.remove(0);
        let target = assert_target(&title_hit, title, SearchTargetField::Title, "Watermelon");
        assert_eq!((target.start, target.end), (3, 13));
        let tags_hit = index.search("pineap", 20, 0).unwrap().results.remove(0);
        let target = assert_target(&tags_hit, tags, SearchTargetField::Tags, "pineapple");
        assert_eq!((target.start, target.end), (11, 20));
        let cjk_tags = index.search("東京大学", 20, 0).unwrap().results.remove(0);
        assert_target(&cjk_tags, tags, SearchTargetField::Tags, "東京大学");
    }

    #[test]
    fn title_and_tag_targets_bound_unicode_text_and_context() {
        let vault = TestVault::new();
        let long_title = format!("heading{}", "é".repeat(300));
        let long_tag = format!("category{}", "é".repeat(300));
        let title = format!("{} {long_title} {}", "🦊".repeat(40), "🌊".repeat(40));
        let tags = format!(
            "{} {long_tag} {} pineapple {}",
            "🦊".repeat(40),
            "🌊".repeat(40),
            "🚀".repeat(40)
        );
        fs::write(
            vault.0.join("bounded.md"),
            format!("---\ntitle: '{title}'\ntags: ['{tags}']\n---\nPlain content."),
        )
        .unwrap();
        let index = vault.index();
        index.rebuild(vault.0.to_str().unwrap()).unwrap();
        for (query, source, field, word) in [
            ("head", &title, SearchTargetField::Title, &long_title),
            ("categ", &tags, SearchTargetField::Tags, &long_tag),
        ] {
            let result = index.search(query, 20, 0).unwrap().results.remove(0);
            let matched: String = word.chars().take(SNIPPET_CHARS).collect();
            let target = assert_target(&result, source, field, &matched);
            assert_eq!(target.start, 81);
            assert_eq!(target.end, 81 + SNIPPET_CHARS);
            assert_eq!(target.before, format!("{} ", "🦊".repeat(31)));
            assert_eq!(target.after, "é".repeat(TARGET_CONTEXT_CHARS));
        }
        let result = index.search("pineap", 20, 0).unwrap().results.remove(0);
        let target = assert_target(&result, &tags, SearchTargetField::Tags, "pineapple");
        assert_eq!(target.before, format!("{} ", "🌊".repeat(31)));
        assert_eq!(target.after, format!(" {}", "🚀".repeat(31)));
    }

    #[test]
    fn stored_snippets_survive_reopen_and_refresh_without_reading_result_files() {
        let vault = TestVault::new();
        let path = write_search_note(&vault, "cached.md", "Archive", "🦊 pineapple context");
        let index = vault.index();
        index.rebuild(vault.0.to_str().unwrap()).unwrap();
        #[cfg(desktop)]
        let index = {
            drop(index);
            vault.index()
        };
        // The cached hit and context remain usable even if the source has become
        // temporarily unreadable. Refreshing later must replace both together.
        fs::write(&path, [0xff]).unwrap();
        let cached = index.search("pineap", 20, 0).unwrap().results.remove(0);
        assert_eq!(cached.snippet, "🦊 pineapple context");
        assert_eq!(
            highlighted(&cached.snippet, &cached.snippet_matches),
            vec!["pineapple"]
        );
        assert_target(
            &cached,
            "🦊 pineapple context",
            SearchTargetField::Body,
            "pineapple",
        );
        write_search_note(
            &vault,
            "cached.md",
            "Archive",
            "🦊 watermelon changed context",
        );
        index.refresh_vault(vault.0.to_str().unwrap()).unwrap();
        assert_eq!(index.search("pineap", 20, 0).unwrap().total, 0);
        let refreshed = index.search("watermelno", 20, 0).unwrap().results.remove(0);
        assert_eq!(refreshed.snippet, "🦊 watermelon changed context");
        assert_eq!(
            highlighted(&refreshed.snippet, &refreshed.snippet_matches),
            vec!["watermelon"]
        );
        assert_target(
            &refreshed,
            "🦊 watermelon changed context",
            SearchTargetField::Body,
            "watermelon",
        );
        index.refresh_vault(vault.0.to_str().unwrap()).unwrap();
        assert_eq!(index.search("watermelno", 20, 0).unwrap().total, 1);
    }

    #[cfg(desktop)]
    #[test]
    fn reopening_waits_for_writer_shutdown_after_the_last_arc_disappears() {
        let vault = TestVault::new();
        let vault_path = vault.0.to_str().unwrap().to_owned();
        let path = vault.0.join("note.md").to_str().unwrap().to_owned();
        fs::write(&path, "watermelon").unwrap();
        let state = Arc::new(crate::state::AppState::new(
            crate::types::AppConfig::default(),
        ));
        let index = state.search_index_for_vault(&vault_path).unwrap();
        index.rebuild(&vault_path).unwrap();
        let weak = Arc::downgrade(&index);
        let shutdown = index.shutdown_signal();

        // Hold Drop before it releases the writer, while allowing the final Arc
        // count to reach zero. This is the window a Weak-only cache cannot cover.
        let paused = shutdown.finished.lock();
        let dropping = std::thread::spawn(move || drop(index));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while weak.strong_count() != 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "last Arc was not dropped"
            );
            std::thread::yield_now();
        }
        assert!(SearchIndex::new(&vault_path).is_err());
        assert!(!shutdown.is_finished());

        let reopening = std::thread::spawn(move || state.search_index_for_vault(&vault_path));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !shutdown
            .wait_started
            .load(std::sync::atomic::Ordering::Acquire)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "reopen did not wait for writer shutdown"
            );
            std::thread::yield_now();
        }
        drop(paused);
        dropping.join().unwrap();
        let reopened = reopening.join().unwrap().unwrap();
        assert!(shutdown.is_finished());
        assert_eq!(result_paths(&reopened, "watermelon"), vec![path]);
    }

    #[test]
    fn coalesced_batch_reads_latest_note_contents() {
        let vault = TestVault::new();
        let index = vault.index();
        let path = vault.0.join("note.md").to_str().unwrap().to_owned();

        // Reserve the worker so the batch boundary is controlled by the test.
        // Draining is synchronous: no assertion depends on a timing deadline.
        index.queue.lock().running = true;
        for content in ["pineapple", "strawberry", "watermelon"] {
            fs::write(&path, content).unwrap();
            index.queue_index_note(&path).unwrap();
        }
        assert_eq!(index.queue.lock().pending.len(), 1);
        index.drain_index_queue();

        assert_eq!(result_paths(&index, "watermelon"), vec![path]);
        assert!(result_paths(&index, "pineapple").is_empty());
        assert!(result_paths(&index, "strawberry").is_empty());
        assert!(!index.queue.lock().running);
    }

    #[test]
    fn queued_moves_and_deletions_do_not_resurrect_old_paths() {
        let vault = TestVault::new();
        let index = vault.index();
        let old_path = vault.0.join("old.md").to_str().unwrap().to_owned();
        let new_path = vault.0.join("new.md").to_str().unwrap().to_owned();
        let deleted_path = vault.0.join("deleted.md").to_str().unwrap().to_owned();
        for path in [&old_path, &deleted_path] {
            fs::write(path, "watermelon").unwrap();
        }
        index.rebuild(vault.0.to_str().unwrap()).unwrap();

        index.queue.lock().running = true;
        index.queue_index_note(&old_path).unwrap();
        index.queue_index_note(&deleted_path).unwrap();
        fs::rename(&old_path, &new_path).unwrap();
        fs::remove_file(&deleted_path).unwrap();
        index.remove_note(&old_path).unwrap();
        index.queue_index_note(&new_path).unwrap();
        index.drain_index_queue();

        assert_eq!(result_paths(&index, "watermelon"), vec![new_path]);
    }

    #[test]
    fn unreadable_note_does_not_block_other_notes_or_later_edits() {
        let vault = TestVault::new();
        let index = vault.index();
        let bad_path = vault.0.join("bad.md").to_str().unwrap().to_owned();
        let good_path = vault.0.join("good.md").to_str().unwrap().to_owned();
        fs::write(&bad_path, [0xff]).unwrap();
        fs::write(&good_path, "watermelon").unwrap();

        index.queue.lock().running = true;
        index.queue_index_note(&bad_path).unwrap();
        index.queue_index_note(&good_path).unwrap();
        index.drain_index_queue();
        assert_eq!(result_paths(&index, "watermelon"), vec![good_path.clone()]);
        assert!(!index.queue.lock().running);

        fs::write(&bad_path, "pineapple").unwrap();
        fs::write(&good_path, "strawberry").unwrap();
        index.queue.lock().running = true;
        index.queue_index_note(&bad_path).unwrap();
        index.queue_index_note(&good_path).unwrap();
        index.drain_index_queue();
        assert_eq!(result_paths(&index, "pineapple"), vec![bad_path]);
        assert_eq!(result_paths(&index, "strawberry"), vec![good_path]);
        assert!(result_paths(&index, "watermelon").is_empty());
    }

    #[test]
    fn failed_batch_releases_worker_for_subsequent_updates() {
        let vault = TestVault::new();
        let index = vault.index();
        let path = vault.0.join("note.md").to_str().unwrap().to_owned();
        fs::write(&path, "watermelon").unwrap();
        let writer = index.writer.lock().take();

        index.queue.lock().running = true;
        index.queue_index_note(&path).unwrap();
        index.drain_index_queue();
        assert!(!index.queue.lock().running);
        assert!(result_paths(&index, "watermelon").is_empty());

        *index.writer.lock() = writer;
        index.queue.lock().running = true;
        index.queue_index_note(&path).unwrap();
        index.drain_index_queue();
        assert_eq!(result_paths(&index, "watermelon"), vec![path]);
        assert!(!index.queue.lock().running);
    }

    #[test]
    fn unchanged_refresh_preserves_commit_and_cached_results() {
        let vault = TestVault::new();
        let path = vault.0.join("note.md").to_str().unwrap().to_owned();
        fs::write(&path, "watermelon").unwrap();
        let index = vault.index();
        index.rebuild(vault.0.to_str().unwrap()).unwrap();
        let committed = index.index.load_metas().unwrap().opstamp;
        #[cfg(desktop)]
        let index = {
            drop(index);
            vault.index()
        };
        assert_eq!(result_paths(&index, "watermelon"), vec![path.clone()]);

        index.queue.lock().running = true;
        index
            .queue_vault_refresh(vault.0.to_str().unwrap())
            .unwrap();
        index
            .queue_vault_refresh(vault.0.to_str().unwrap())
            .unwrap();
        // Cached search is available before reconciliation is allowed to run.
        assert_eq!(result_paths(&index, "watermelon"), vec![path.clone()]);
        index.drain_index_queue();
        assert_eq!(index.index.load_metas().unwrap().opstamp, committed);
        assert_eq!(result_paths(&index, "watermelon"), vec![path]);
    }

    fn set_modified(path: &str, nanos: u32) {
        let modified = UNIX_EPOCH + Duration::new(1_700_000_000, nanos);
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
    }

    #[test]
    fn refresh_detects_subsecond_edits_size_changes_new_notes_and_deletions() {
        let vault = TestVault::new();
        let index = vault.index();
        let edited = vault.0.join("edited.md").to_str().unwrap().to_owned();
        let resized = vault.0.join("resized.md").to_str().unwrap().to_owned();
        let deleted = vault.0.join("deleted.md").to_str().unwrap().to_owned();
        let added = vault.0.join("added.md").to_str().unwrap().to_owned();
        for path in [&edited, &resized, &deleted] {
            fs::write(path, "pineapple").unwrap();
            set_modified(path, 100_000_000);
        }
        index.rebuild(vault.0.to_str().unwrap()).unwrap();

        // Same byte length and second; only subsecond precision detects this edit.
        fs::write(&edited, "blueberry").unwrap();
        set_modified(&edited, 900_000_000);
        fs::write(&resized, "strawberry").unwrap();
        set_modified(&resized, 100_000_000);
        fs::remove_file(&deleted).unwrap();
        fs::write(&added, "watermelon").unwrap();
        index.refresh_vault(vault.0.to_str().unwrap()).unwrap();

        assert_eq!(result_paths(&index, "blueberry"), vec![edited]);
        assert_eq!(result_paths(&index, "strawberry"), vec![resized]);
        assert_eq!(result_paths(&index, "watermelon"), vec![added]);
        assert!(result_paths(&index, "pineapple").is_empty());
        let committed = index.index.load_metas().unwrap().opstamp;
        index.refresh_vault(vault.0.to_str().unwrap()).unwrap();
        assert_eq!(index.index.load_metas().unwrap().opstamp, committed);
    }

    #[test]
    fn refresh_and_queued_edits_use_latest_disk_state() {
        let vault = TestVault::new();
        let index = vault.index();
        let edited = vault.0.join("edited.md").to_str().unwrap().to_owned();
        let deleted = vault.0.join("deleted.md").to_str().unwrap().to_owned();
        for path in [&edited, &deleted] {
            fs::write(path, "pineapple").unwrap();
        }
        index.rebuild(vault.0.to_str().unwrap()).unwrap();
        index.queue.lock().running = true;
        index
            .queue_vault_refresh(vault.0.to_str().unwrap())
            .unwrap();
        index.queue_index_note(&edited).unwrap();
        index.queue_index_note(&deleted).unwrap();
        fs::write(&edited, "watermelon").unwrap();
        fs::remove_file(&deleted).unwrap();
        index.remove_note(&deleted).unwrap();
        index.drain_index_queue();

        assert_eq!(result_paths(&index, "watermelon"), vec![edited]);
        assert!(result_paths(&index, "pineapple").is_empty());
        assert!(!index.queue.lock().running);
    }

    #[test]
    fn failed_refresh_preserves_cached_notes_and_later_queue_work() {
        let vault = TestVault::new();
        let index = vault.index();
        let path = vault.0.join("note.md").to_str().unwrap().to_owned();
        fs::write(&path, "pineapple").unwrap();
        index.rebuild(vault.0.to_str().unwrap()).unwrap();
        fs::write(&path, [0xff]).unwrap();
        let committed = index.index.load_metas().unwrap().opstamp;
        index.refresh_vault(vault.0.to_str().unwrap()).unwrap();
        assert_eq!(index.index.load_metas().unwrap().opstamp, committed);
        assert_eq!(result_paths(&index, "pineapple"), vec![path.clone()]);

        index.queue.lock().running = true;
        index
            .queue_vault_refresh(vault.0.join("missing").to_str().unwrap())
            .unwrap();
        fs::write(&path, "watermelon").unwrap();
        index.queue_index_note(&path).unwrap();
        index.drain_index_queue();
        assert_eq!(result_paths(&index, "watermelon"), vec![path]);
        assert!(result_paths(&index, "pineapple").is_empty());
    }

    #[test]
    fn first_cold_query_waits_for_initial_reconciliation() {
        let vault = TestVault::new();
        let index = vault.index();
        let path = vault.0.join("note.md").to_str().unwrap().to_owned();
        fs::write(&path, "watermelon").unwrap();
        index.queue.lock().running = true;
        index
            .queue_vault_refresh(vault.0.to_str().unwrap())
            .unwrap();
        let worker = Arc::clone(&index);
        let handle = std::thread::spawn(move || worker.drain_index_queue());
        assert_eq!(result_paths(&index, "watermelon"), vec![path]);
        handle.join().unwrap();
    }

    #[test]
    fn failed_initial_refresh_releases_query_and_allows_later_work() {
        let vault = TestVault::new();
        let index = vault.index();
        let writer = index.writer.lock().take();
        index.queue.lock().running = true;
        index
            .queue_vault_refresh(vault.0.to_str().unwrap())
            .unwrap();
        let worker = Arc::clone(&index);
        let handle = std::thread::spawn(move || worker.drain_index_queue());
        assert!(result_paths(&index, "watermelon").is_empty());
        handle.join().unwrap();
        *index.writer.lock() = writer;
        let path = vault.0.join("note.md").to_str().unwrap().to_owned();
        fs::write(&path, "watermelon").unwrap();
        index.queue.lock().running = true;
        index.queue_index_note(&path).unwrap();
        index.drain_index_queue();
        assert_eq!(result_paths(&index, "watermelon"), vec![path]);
    }
}
