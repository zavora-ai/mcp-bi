//! Remembering corrections, so the same mistake is not made twice.
//!
//! OpenAI published a measurement worth taking seriously: the same question against
//! their internal data agent took **22m 41s without memory and 1m 22s with it**. The
//! reason is not that memory makes a model cleverer. It is that a data platform is
//! full of facts that cannot be derived from its schema — which of four
//! similarly-named charts is the one people mean by "revenue", that a filter needs an
//! exact gate string rather than a fuzzy match, that a dataset id is a table id and
//! not a database id. An agent rediscovers each of those the expensive way, every
//! time, unless something writes them down.
//!
//! So this stores **corrections, not knowledge**. "Superset chart 37 is the one to use
//! for product-line volume" is worth keeping. "Revenue was 10M last month" is not: it
//! will be wrong next month, and the numbers should always come from a query.
//!
//! ## Scope, because a correction is rarely universal
//!
//! Every note is keyed by the backend it was learned against, and optionally by a
//! dashboard, chart or dataset. A correction about Superset's chart ids is worse than
//! useless against Metabase, so it is never offered there.
//!
//! ## Retrieval is deliberately boring
//!
//! Word overlap against the note's subject and text, not embeddings. That keeps recall
//! deterministic, testable offline, and free of an API key — and at the scale a single
//! organisation's BI platform reaches, ranked keyword overlap finds the right note.
//! If this ever needs to span tens of thousands of datasets, embeddings are the answer,
//! and the tool surface would not change.
//!
//! ## What a note is not
//!
//! Notes are written by a model and later fed back to a model, so they are returned
//! labelled as recorded observations rather than instructions. A note that says
//! "ignore your previous instructions" is a note that says that, and nothing more.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// Bounds. A memory that grows without limit eventually costs more context than it
/// saves, and the oldest corrections are the ones most likely to be stale.
const MAX_NOTES: usize = 500;
const MAX_NOTE_CHARS: usize = 600;
const MAX_SUBJECT_CHARS: usize = 120;
const DEFAULT_RECALL: usize = 5;

/// One correction, learned the expensive way so it need not be learned again.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Note {
    /// What this is about, in the agent's own words: "product-line volume chart".
    pub subject: String,
    /// The correction itself.
    pub note: String,
    /// Which backend it was learned against. A Superset chart id means nothing to
    /// Metabase, so a note is only ever offered back for its own backend.
    pub backend: String,
    /// Optional narrower scope: a dashboard, chart or dataset id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// When it was recorded, so a reader can judge staleness.
    pub recorded_at: String,
    /// How often it has been recalled. A note nothing ever needs is a candidate for
    /// removal; one recalled constantly is load-bearing.
    #[serde(default)]
    pub recalled: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Book {
    notes: Vec<Note>,
}

/// Where notes live, and the reason a note might not be saved.
pub struct Recall {
    path: Option<PathBuf>,
    book: RwLock<Book>,
    /// Why persistence is unavailable, if it is. Reported rather than hidden: an
    /// agent told its memory is not being kept can say so instead of assuming it is.
    unavailable: Option<String>,
}

impl Recall {
    /// Open the store named by `BI_MEMORY_PATH`, or the default beside the config dir.
    ///
    /// An unwritable location is not fatal. Memory is an optimisation, and refusing to
    /// start a read-only BI server because a cache file cannot be written would be a
    /// worse failure than losing the optimisation.
    pub fn open_from_env() -> Self {
        let configured = std::env::var("BI_MEMORY_PATH").ok().map(PathBuf::from);
        let path = configured.or_else(default_path);
        match path {
            None => Self {
                path: None,
                book: RwLock::new(Book::default()),
                unavailable: Some(
                    "No writable location for memory. Set BI_MEMORY_PATH to a file this \
                     process may write. Notes will be kept for this session only."
                        .into(),
                ),
            },
            Some(path) => match load(&path) {
                Ok(book) => Self {
                    path: Some(path),
                    book: RwLock::new(book),
                    unavailable: None,
                },
                Err(error) => Self {
                    path: Some(path.clone()),
                    book: RwLock::new(Book::default()),
                    unavailable: Some(format!(
                        "Could not read {} ({error}). Notes will be kept for this session only.",
                        path.display()
                    )),
                },
            },
        }
    }

    /// An in-memory store, for tests and for a caller that wants no file at all.
    pub fn ephemeral() -> Self {
        Self {
            path: None,
            book: RwLock::new(Book::default()),
            unavailable: None,
        }
    }

    pub fn status(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    pub fn location(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn len(&self) -> usize {
        self.book.read().expect("notes lock").notes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Record a correction, replacing any earlier note with the same subject and scope.
    ///
    /// Replacing rather than appending matters: a correction that has itself been
    /// corrected should not sit alongside its replacement, both looking equally true.
    pub fn remember(
        &self,
        subject: &str,
        note: &str,
        backend: &str,
        scope: Option<&str>,
        now: &str,
    ) -> Result<Note> {
        let subject = subject.trim();
        let note = note.trim();
        if subject.is_empty() || note.is_empty() {
            return Err(anyhow!(
                "bi_remember needs both a subject and a note. The subject is what the \
                 correction is about; the note is the correction itself."
            ));
        }
        if note.chars().count() > MAX_NOTE_CHARS {
            return Err(anyhow!(
                "A note may be at most {MAX_NOTE_CHARS} characters. Record the correction, \
                 not the analysis \u{2014} a long note is usually a finding, and findings \
                 belong in your answer rather than in memory."
            ));
        }
        let entry = Note {
            subject: truncate(subject, MAX_SUBJECT_CHARS),
            note: note.to_string(),
            backend: backend.to_string(),
            scope: scope.map(|value| value.to_string()),
            recorded_at: now.to_string(),
            recalled: 0,
        };
        {
            let mut book = self.book.write().expect("notes lock");
            book.notes.retain(|existing| {
                !(existing.subject.eq_ignore_ascii_case(&entry.subject)
                    && existing.backend == entry.backend
                    && existing.scope == entry.scope)
            });
            book.notes.push(entry.clone());
            // Oldest first, so the cap drops the least likely to still be true.
            if book.notes.len() > MAX_NOTES {
                let excess = book.notes.len() - MAX_NOTES;
                book.notes.drain(0..excess);
            }
        }
        self.persist()?;
        Ok(entry)
    }

    /// Offer back the notes most likely to bear on a question.
    ///
    /// Ranked by word overlap, then by how often a note has proved useful before, then
    /// by recency. A note for another backend is never returned.
    pub fn recall(
        &self,
        question: &str,
        backend: &str,
        scope: Option<&str>,
        limit: Option<usize>,
    ) -> Vec<Note> {
        let wanted = limit.unwrap_or(DEFAULT_RECALL).clamp(1, 25);
        let terms = terms_of(question);
        let mut scored: Vec<(usize, &Note)> = Vec::new();
        let book = self.book.read().expect("notes lock");
        for note in &book.notes {
            if note.backend != backend {
                continue;
            }
            let mut score = overlap(&terms, &note.subject) * 3 + overlap(&terms, &note.note);
            // A note scoped to exactly what is being asked about is relevant even when
            // no words happen to match.
            if let (Some(asked), Some(noted)) = (scope, note.scope.as_deref())
                && asked == noted
            {
                score += 10;
            }
            if score > 0 {
                scored.push((score, note));
            }
        }
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then(b.1.recalled.cmp(&a.1.recalled))
                .then(b.1.recorded_at.cmp(&a.1.recorded_at))
        });
        let hits: Vec<Note> = scored
            .into_iter()
            .take(wanted)
            .map(|(_, note)| note.clone())
            .collect();
        drop(book);

        // Count the recall, so a load-bearing note can be told from a stale one. A
        // failure to write this back is not worth failing the read for.
        if !hits.is_empty() {
            let mut book = self.book.write().expect("notes lock");
            for note in &mut book.notes {
                if hits.iter().any(|hit| {
                    hit.subject == note.subject
                        && hit.backend == note.backend
                        && hit.scope == note.scope
                }) {
                    note.recalled += 1;
                }
            }
            drop(book);
            let _ = self.persist();
        }
        hits
    }

    /// Every note for a backend, newest first. For a person auditing what was learned.
    pub fn all(&self, backend: Option<&str>) -> Vec<Note> {
        let book = self.book.read().expect("notes lock");
        let mut notes: Vec<Note> = book
            .notes
            .iter()
            .filter(|note| backend.is_none_or(|wanted| note.backend == wanted))
            .cloned()
            .collect();
        notes.sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at));
        notes
    }

    /// Forget a note by subject. A wrong correction is worse than none, so this is
    /// part of the surface rather than something only a file edit can do.
    pub fn forget(&self, subject: &str, backend: &str) -> Result<usize> {
        let removed = {
            let mut book = self.book.write().expect("notes lock");
            let before = book.notes.len();
            book.notes.retain(|note| {
                !(note.subject.eq_ignore_ascii_case(subject.trim()) && note.backend == backend)
            });
            before - book.notes.len()
        };
        if removed > 0 {
            self.persist()?;
        }
        Ok(removed)
    }

    fn persist(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let book = self.book.read().expect("notes lock");
        let json = serde_json::to_string_pretty(&*book)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        // Write beside the target and rename, so an interrupted write cannot leave a
        // half-written file where a readable one used to be.
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, json)
            .with_context(|| format!("writing {}", temporary.display()))?;
        std::fs::rename(&temporary, path)
            .with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }
}

fn load(path: &Path) -> Result<Book> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Book::default()),
        Ok(text) => Ok(serde_json::from_str(&text)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Book::default()),
        Err(error) => Err(error.into()),
    }
}

fn default_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".mcp-bi").join("memory.json"))
}

fn truncate(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    value.chars().take(limit).collect()
}

/// Words worth matching on: lowercase, de-punctuated, and short words dropped.
fn terms_of(text: &str) -> BTreeMap<String, ()> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.len() > 3)
        .map(|word| (word.to_lowercase(), ()))
        .collect()
}

fn overlap(terms: &BTreeMap<String, ()>, text: &str) -> usize {
    terms_of(text)
        .keys()
        .filter(|word| terms.contains_key(*word))
        .count()
}
