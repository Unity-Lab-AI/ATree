//! Git history extraction and analysis.
//!
//! Extracts commit history, blame data, and author statistics from a git repository
//! using the `git2` crate (libgit2 bindings). All data is persisted to the SQLite
//! graph store alongside the structural code intelligence.
//!
//! # Data model
//!
//! ```text
//! commits: hash, author_name, author_email, timestamp, message, repo_path
//! file_commits: file_id, commit_hash, lines_added, lines_removed, is_creation
//! blame_lines: file_id, line_number, commit_hash, author_name, last_changed_at
//! authors: email, name, commit_count, lines_added, lines_removed, first_commit, last_commit
//! ```

use git2::{Repository, Diff, Oid};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;

// ── Data structures ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CommitRecord {
    pub hash: String,
    pub author_name: String,
    pub author_email: String,
    pub timestamp: i64,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct FileCommitRecord {
    pub path: String,
    pub commit_hash: String,
    pub lines_added: i64,
    pub lines_removed: i64,
    pub is_creation: bool,
}

#[derive(Debug, Clone)]
pub struct BlameLine {
    pub line_number: usize,
    pub commit_hash: String,
    pub author_name: String,
    pub author_email: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone)]
pub struct AuthorStats {
    pub email: String,
    pub name: String,
    pub commit_count: i64,
    pub lines_added: i64,
    pub lines_removed: i64,
    pub first_commit: i64,
    pub last_commit: i64,
}

/// Configuration for git history extraction.
#[derive(Debug, Clone)]
pub struct GitHistoryConfig {
    /// Maximum number of commits to process (0 = all).
    pub max_commits: usize,
    /// Maximum age in days (0 = no limit).
    pub max_age_days: i64,
    /// Whether to extract per-line blame data (expensive for large files).
    pub include_blame: bool,
    /// Only extract blame for files with lines <= this threshold.
    pub blame_file_line_threshold: usize,
}

impl Default for GitHistoryConfig {
    fn default() -> Self {
        Self {
            max_commits: 0,          // all commits
            max_age_days: 0,         // no age limit
            include_blame: false,    // blame is expensive; enable on demand
            blame_file_line_threshold: 2000,
        }
    }
}

/// Lightweight config for CI / fast scans.
pub fn git_history_fast_config() -> GitHistoryConfig {
    GitHistoryConfig {
        max_commits: 200,
        max_age_days: 90,
        include_blame: false,
        blame_file_line_threshold: 0,
    }
}

// ── Extraction ─────────────────────────────────────────────────────────────

/// Extract git history from the repository at `repo_root` and persist to the
/// SQLite connection. This is the main entry point.
///
/// # Strategy
///
/// 1. Walk the commit log (reverse chronological via `revwalk`)
/// 2. For each commit, diff against its parent(s) to get changed files
/// 3. Accumulate per-file change stats (added/removed flags)
/// 4. Build author statistics
/// 5. Persist everything in a single transaction
///
/// Blame is NOT extracted during indexing — it's done on-demand per file
/// when the user queries `git-blame <path>`, because batch blame across
/// thousands of files is too slow (can take 15+ minutes).
pub fn extract_and_persist(
    repo_root: &Path,
    conn: &Connection,
    config: &GitHistoryConfig,
) -> anyhow::Result<GitHistoryStats> {
    let repo = Repository::discover(repo_root)
        .map_err(|e| anyhow::anyhow!("No git repo found at {}: {}", repo_root.display(), e))?;

    // Skip if git history already extracted (idempotent)
    let existing: i64 = conn.query_row("SELECT COUNT(*) FROM commits", [], |r| r.get(0)).unwrap_or(0);
    if existing > 0 {
        return Ok(GitHistoryStats {
            commits_processed: existing as usize,
            ..Default::default()
        });
    }

    let mut stats = GitHistoryStats::default();

    let commits = collect_commits(&repo, config, &mut stats)?;
    let file_commits = diff_commits(&repo, &commits, &mut stats)?;
    let authors = aggregate_authors(&commits, &file_commits);

    persist_git_data(conn, &commits, &file_commits, &authors)?;

    stats.commits_processed = commits.len();
    stats.files_tracked = file_commits.len();
    stats.authors_tracked = authors.len();
    Ok(stats)
}

/// Extract blame for a single file on-demand. This is called by the
/// `git-blame <path>` query handler, not during bulk indexing.
pub fn extract_blame_for_file(
    repo_root: &Path,
    rel_path: &str,
) -> anyhow::Result<Vec<BlameLine>> {
    let repo = Repository::discover(repo_root)
        .map_err(|e| anyhow::anyhow!("No git repo: {}", e))?;

    if let Some(wd) = repo.workdir() {
        let full_path = wd.join(rel_path);
        if !full_path.exists() {
            return Ok(Vec::new());
        }
    }

    let mut blame_opts = git2::BlameOptions::new();
    let blame = repo.blame_file(Path::new(rel_path), Some(&mut blame_opts))
        .map_err(|e| anyhow::anyhow!("blame error for {}: {}", rel_path, e))?;

    let mut lines = Vec::new();
    for hunk in blame.iter() {
        let commit_id = hunk.final_commit_id();
        let author = hunk.final_signature();
        let hunk_lines = hunk.lines_in_hunk();
        for i in 0..hunk_lines {
            lines.push(BlameLine {
                line_number: hunk.final_start_line() + i,
                commit_hash: commit_id.to_string(),
                author_name: author.name().unwrap_or("unknown").to_string(),
                author_email: author.email().unwrap_or("unknown").to_string(),
                timestamp: author.when().seconds(),
            });
        }
    }
    Ok(lines)
}

#[derive(Debug, Default)]
pub struct GitHistoryStats {
    pub commits_processed: usize,
    pub files_tracked: usize,
    pub authors_tracked: usize,
    pub errors: Vec<String>,
}

// ── Commit collection ─────────────────────────────────────────────────────

fn collect_commits(
    repo: &Repository,
    config: &GitHistoryConfig,
    stats: &mut GitHistoryStats,
) -> anyhow::Result<Vec<CommitRecord>> {
    let mut revwalk = repo.revwalk()?;
    revwalk.push_head()?;
    revwalk.set_sorting(git2::Sort::TIME)?;

    let mut commits = Vec::new();
    let cutoff = if config.max_age_days > 0 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        now - (config.max_age_days * 86400)
    } else {
        0
    };

    for (idx, oid_result) in revwalk.enumerate() {
        if config.max_commits > 0 && idx >= config.max_commits {
            break;
        }

        let oid = match oid_result {
            Ok(id) => id,
            Err(e) => {
                stats.errors.push(format!("revwalk error: {}", e));
                continue;
            }
        };

        let commit = match repo.find_commit(oid) {
            Ok(c) => c,
            Err(e) => {
                stats.errors.push(format!("commit {} not found: {}", oid, e));
                continue;
            }
        };

        let time = commit.time().seconds();
        if cutoff > 0 && time < cutoff {
            break; // commits are sorted by time descending, so we can stop
        }

        let author = commit.author();
        commits.push(CommitRecord {
            hash: oid.to_string(),
            author_name: author.name().unwrap_or("unknown").to_string(),
            author_email: author.email().unwrap_or("unknown").to_string(),
            timestamp: time,
            message: commit.summary().unwrap_or("").to_string(),
        });
    }

    Ok(commits)
}

// ── Diff extraction ───────────────────────────────────────────────────────

fn diff_commits(
    repo: &Repository,
    commits: &[CommitRecord],
    stats: &mut GitHistoryStats,
) -> anyhow::Result<Vec<FileCommitRecord>> {
    let mut file_commits: Vec<FileCommitRecord> = Vec::new();

    for commit_rec in commits {
        let oid = match Oid::from_str(&commit_rec.hash) {
            Ok(id) => id,
            Err(e) => {
                stats.errors.push(format!("invalid oid {}: {}", commit_rec.hash, e));
                continue;
            }
        };

        let commit = match repo.find_commit(oid) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let commit_tree = match commit.tree() {
            Ok(t) => t,
            Err(_) => continue,
        };

        // Diff against parent(s). For merge commits, diff against first parent.
        let parent_tree = if commit.parent_count() > 0 {
            match commit.parent(0) {
                Ok(parent) => match parent.tree() {
                    Ok(t) => Some(t),
                    Err(_) => None,
                },
                Err(_) => None,
            }
        } else {
            None // root commit — diff against empty tree
        };

        let diff = match repo.diff_tree_to_tree(
            parent_tree.as_ref(),
            Some(&commit_tree),
            None,
        ) {
            Ok(d) => d,
            Err(e) => {
                stats.errors.push(format!("diff error for {}: {}", commit_rec.hash, e));
                continue;
            }
        };

        let mut fc = diff_to_file_commits(&diff, &commit_rec.hash);
        file_commits.append(&mut fc);
    }

    Ok(file_commits)
}

fn diff_to_file_commits(diff: &Diff, commit_hash: &str) -> Vec<FileCommitRecord> {
    let mut records = Vec::new();

    for delta in diff.deltas() {
        let path = delta.new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .and_then(|p| p.to_str())
            .unwrap_or("")
            .to_string();
        if path.is_empty() { continue; }

        let is_creation = matches!(delta.status(), git2::Delta::Added);

        records.push(FileCommitRecord {
            path,
            commit_hash: commit_hash.to_string(),
            lines_added: 0,
            lines_removed: 0,
            is_creation,
        });
    }

    records
}

// ── Author aggregation ────────────────────────────────────────────────────

fn aggregate_authors(
    commits: &[CommitRecord],
    file_commits: &[FileCommitRecord],
) -> Vec<AuthorStats> {
    let mut author_map: HashMap<String, AuthorStats> = HashMap::new();

    // Count commits per author
    for commit in commits {
        let entry = author_map.entry(commit.author_email.clone()).or_insert_with(|| AuthorStats {
            email: commit.author_email.clone(),
            name: commit.author_name.clone(),
            commit_count: 0,
            lines_added: 0,
            lines_removed: 0,
            first_commit: commit.timestamp,
            last_commit: commit.timestamp,
        });
        entry.commit_count += 1;
        entry.first_commit = entry.first_commit.min(commit.timestamp);
        entry.last_commit = entry.last_commit.max(commit.timestamp);
        // Keep the most recent name (handles author renaming)
        entry.name = commit.author_name.clone();
    }

    // Aggregate line changes per author (approximate: attribute to commit author)
    // We need to join file_commits with commits by hash to get author
    let hash_to_email: HashMap<&str, &str> = commits
        .iter()
        .map(|c| (c.hash.as_str(), c.author_email.as_str()))
        .collect();

    for fc in file_commits {
        if let Some(&email) = hash_to_email.get(fc.commit_hash.as_str()) {
            if let Some(stats) = author_map.get_mut(email) {
                stats.lines_added += fc.lines_added;
                stats.lines_removed += fc.lines_removed;
            }
        }
    }

    author_map.into_values().collect()
}

// ── Persistence ───────────────────────────────────────────────────────────

fn persist_git_data(
    conn: &Connection,
    commits: &[CommitRecord],
    file_commits: &[FileCommitRecord],
    authors: &[AuthorStats],
) -> anyhow::Result<()> {
    let tx = conn.unchecked_transaction()?;

    // Create git tables if they don't exist
    tx.execute_batch("
        CREATE TABLE IF NOT EXISTS commits (
            hash TEXT PRIMARY KEY,
            author_name TEXT NOT NULL,
            author_email TEXT NOT NULL,
            timestamp INTEGER NOT NULL,
            message TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_commits_author ON commits(author_email);
        CREATE INDEX IF NOT EXISTS idx_commits_timestamp ON commits(timestamp);

        CREATE TABLE IF NOT EXISTS file_commits (
            id INTEGER PRIMARY KEY,
            file_path TEXT NOT NULL,
            commit_hash TEXT NOT NULL REFERENCES commits(hash),
            lines_added INTEGER NOT NULL DEFAULT 0,
            lines_removed INTEGER NOT NULL DEFAULT 0,
            is_creation INTEGER NOT NULL DEFAULT 0,
            UNIQUE(file_path, commit_hash)
        );
        CREATE INDEX IF NOT EXISTS idx_file_commits_path ON file_commits(file_path);
        CREATE INDEX IF NOT EXISTS idx_file_commits_hash ON file_commits(commit_hash);

        CREATE TABLE IF NOT EXISTS authors (
            email TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            commit_count INTEGER NOT NULL DEFAULT 0,
            lines_added INTEGER NOT NULL DEFAULT 0,
            lines_removed INTEGER NOT NULL DEFAULT 0,
            first_commit INTEGER NOT NULL,
            last_commit INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_authors_commits ON authors(commit_count DESC);

        CREATE TABLE IF NOT EXISTS blame_lines (
            id INTEGER PRIMARY KEY,
            file_id INTEGER NOT NULL REFERENCES files(id),
            line_number INTEGER NOT NULL,
            commit_hash TEXT NOT NULL,
            author_name TEXT NOT NULL,
            last_changed_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_blame_file ON blame_lines(file_id);
        CREATE INDEX IF NOT EXISTS idx_blame_author ON blame_lines(author_name);
    ")?;

    // Insert commits
    let mut commit_stmt = tx.prepare(
        "INSERT OR REPLACE INTO commits (hash, author_name, author_email, timestamp, message)
         VALUES (?1, ?2, ?3, ?4, ?5)"
    )?;
    for c in commits {
        commit_stmt.execute(params![
            &c.hash, &c.author_name, &c.author_email, c.timestamp, &c.message,
        ])?;
    }
    drop(commit_stmt);

    // Insert file_commits
    let mut fc_stmt = tx.prepare(
        "INSERT OR IGNORE INTO file_commits (file_path, commit_hash, lines_added, lines_removed, is_creation)
         VALUES (?1, ?2, ?3, ?4, ?5)"
    )?;
    for fc in file_commits {
        fc_stmt.execute(params![
            &fc.path, &fc.commit_hash, fc.lines_added, fc.lines_removed,
            fc.is_creation as i64,
        ])?;
    }
    drop(fc_stmt);

    // Insert authors
    let mut author_stmt = tx.prepare(
        "INSERT OR REPLACE INTO authors (email, name, commit_count, lines_added, lines_removed, first_commit, last_commit)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
    )?;
    for a in authors {
        author_stmt.execute(params![
            &a.email, &a.name, a.commit_count, a.lines_added, a.lines_removed,
            a.first_commit, a.last_commit,
        ])?;
    }
    drop(author_stmt);

    tx.commit()?;
    Ok(())
}

// ── Query helpers ─────────────────────────────────────────────────────────

/// Get commit history for a file path.
pub fn get_file_history(conn: &Connection, file_path: &str, limit: usize) -> rusqlite::Result<Vec<CommitRecord>> {
    let mut stmt = conn.prepare(
        "SELECT c.hash, c.author_name, c.author_email, c.timestamp, c.message
         FROM commits c
         JOIN file_commits fc ON fc.commit_hash = c.hash
         WHERE fc.file_path = ?1
         ORDER BY c.timestamp DESC
         LIMIT ?2"
    )?;
    let rows = stmt.query_map(params![file_path, limit as i64], |row| {
        Ok(CommitRecord {
            hash: row.get(0)?,
            author_name: row.get(1)?,
            author_email: row.get(2)?,
            timestamp: row.get(3)?,
            message: row.get(4)?,
        })
    })?;
    rows.collect()
}

/// Get blame data for a file.
pub fn get_file_blame(conn: &Connection, file_id: i64) -> rusqlite::Result<Vec<(usize, String, String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT line_number, author_name, commit_hash, last_changed_at
         FROM blame_lines
         WHERE file_id = ?1
         ORDER BY line_number"
    )?;
    let rows = stmt.query_map([file_id], |row| {
        Ok((
            row.get::<_, i64>(0)? as usize,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    rows.collect()
}

/// Get top authors by commit count.
pub fn get_top_authors(conn: &Connection, limit: usize) -> rusqlite::Result<Vec<AuthorStats>> {
    let mut stmt = conn.prepare(
        "SELECT email, name, commit_count, lines_added, lines_removed, first_commit, last_commit
         FROM authors
         ORDER BY commit_count DESC
         LIMIT ?1"
    )?;
    let rows = stmt.query_map([limit as i64], |row| {
        Ok(AuthorStats {
            email: row.get(0)?,
            name: row.get(1)?,
            commit_count: row.get(2)?,
            lines_added: row.get(3)?,
            lines_removed: row.get(4)?,
            first_commit: row.get(5)?,
            last_commit: row.get(6)?,
        })
    })?;
    rows.collect()
}

/// Get change frequency per file (how often each file is modified).
pub fn get_change_frequency(conn: &Connection, limit: usize) -> rusqlite::Result<Vec<(String, i64, i64)>> {
    // (path, commit_count, total_lines_changed)
    let mut stmt = conn.prepare(
        "SELECT file_path, COUNT(*) as commits, SUM(lines_added + lines_removed) as total_changes
         FROM file_commits
         GROUP BY file_path
         ORDER BY commits DESC
         LIMIT ?1"
    )?;
    let rows = stmt.query_map([limit as i64], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?))
    })?;
    rows.collect()
}

/// Get co-changed files (files that frequently change together).
pub fn get_cochange_frequency(conn: &Connection, file_path: &str, limit: usize) -> rusqlite::Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT fc2.file_path, COUNT(*) as co_changes
         FROM file_commits fc1
         JOIN file_commits fc2 ON fc1.commit_hash = fc2.commit_hash AND fc1.file_path != fc2.file_path
         WHERE fc1.file_path = ?1
         GROUP BY fc2.file_path
         ORDER BY co_changes DESC
         LIMIT ?2"
    )?;
    let rows = stmt.query_map(params![file_path, limit as i64], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    rows.collect()
}

// ── Integrated cross-domain queries ────────────────────────────────────────
// These combine structural code intelligence (symbols, calls, scopes) with
// git history intelligence (commits, authors, co-change).

/// Symbol ownership: who last changed this symbol, how many authors touched it.
pub fn get_symbol_ownership(
    conn: &Connection,
    symbol_name: &str,
) -> rusqlite::Result<Vec<(String, String, i64, i64, String)>> {
    // (symbol_name, author_name, last_changed_ts, num_authors, first_commit_hash)
    let mut stmt = conn.prepare(
        "SELECT s.name,
                c.author_name,
                MAX(c.timestamp) as last_changed,
                COUNT(DISTINCT c.author_name) as author_count,
                MIN(c.hash) as first_commit
         FROM symbols s
         JOIN files f ON f.id = s.file_id
         JOIN file_commits fc ON fc.file_path = f.path
         JOIN commits c ON c.hash = fc.commit_hash
         WHERE s.name = ?1
         GROUP BY s.name, c.author_name
         ORDER BY last_changed DESC
         LIMIT 5"
    )?;
    let rows = stmt.query_map([symbol_name], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?, row.get::<_, i64>(3)?, row.get::<_, String>(4)?))
    })?;
    rows.collect()
}

/// Change risk score: combines git hotspot frequency, author count, and recency.
pub fn get_change_risk(
    conn: &Connection,
    file_path: &str,
) -> rusqlite::Result<Vec<(String, i64, i64, i64, f64)>> {
    // (file_path, commit_count, author_count, days_since_last_change, risk_score)
    let mut stmt = conn.prepare(
        "SELECT fc.file_path,
                COUNT(*) as commit_count,
                COUNT(DISTINCT c.author_name) as author_count,
                (strftime('%s','now') - MAX(c.timestamp)) / 86400 as days_since_last,
                (COUNT(*) * 1.0) / MAX(1, (strftime('%s','now') - MIN(c.timestamp)) / 86400)
                    * COUNT(DISTINCT c.author_name) as risk_score
         FROM file_commits fc
         JOIN commits c ON c.hash = fc.commit_hash
         WHERE fc.file_path = ?1
         GROUP BY fc.file_path"
    )?;
    let rows = stmt.query_map([file_path], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?, row.get::<_, i64>(3)?, row.get::<_, f64>(4)?))
    })?;
    rows.collect()
}

/// Find experts: authors ranked by recency + volume of changes to this file.
pub fn find_experts(
    conn: &Connection,
    file_path: &str,
    limit: usize,
) -> rusqlite::Result<Vec<(String, i64, i64, i64)>> {
    // (author_name, commit_count, lines_changed, last_activity_ts)
    let mut stmt = conn.prepare(
        "SELECT c.author_name,
                COUNT(*) as commits,
                SUM(fc.lines_added + fc.lines_removed) as lines_changed,
                MAX(c.timestamp) as last_active
         FROM file_commits fc
         JOIN commits c ON c.hash = fc.commit_hash
         WHERE fc.file_path = ?1
         GROUP BY c.author_name
         ORDER BY commits DESC, last_active DESC
         LIMIT ?2"
    )?;
    let rows = stmt.query_map(params![file_path, limit as i64], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?, row.get::<_, i64>(3)?))
    })?;
    rows.collect()
}

/// Smart co-change: git co-change history joined with structural proximity.
pub fn get_smart_cochange(
    conn: &Connection,
    symbol_name: &str,
    limit: usize,
) -> rusqlite::Result<Vec<(String, i64, String)>> {
    // (file_path, git_cochanges, signal_type)
    let mut stmt = conn.prepare(
        "SELECT fc2.file_path,
                COUNT(DISTINCT fc1.commit_hash) as git_cochanges,
                'git_only' as signal_type
         FROM file_commits fc1
         JOIN file_commits fc2 ON fc1.commit_hash = fc2.commit_hash
            AND fc1.file_path != fc2.file_path
         JOIN files f1 ON f1.path = fc1.file_path
         JOIN symbols s ON s.file_id = f1.id
         WHERE s.name = ?1
         GROUP BY fc2.file_path
         ORDER BY git_cochanges DESC
         LIMIT ?2"
    )?;
    let rows = stmt.query_map(params![symbol_name, limit as i64], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?))
    })?;
    rows.collect()
}
