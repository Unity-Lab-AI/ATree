//! COBOL/JCL extraction — column-aware line scanner.
//!
//! Modeled after GitNexusRelay's:
//! - `gitnexus/src/core/ingestion/pipeline-phases/cobol.ts`
//! - `gitnexus/src/core/cobol-processor.ts`
//!
//! Column-aware scanner (no regex, no tree-sitter). COBOL fixed-format rules:
//! - Cols 1-6:   indicator area (sequence numbers, comments, etc.)
//! - Col 7:      margin A (DIVISION, SECTION, paragraph names)
//! - Col 8-72:   margin B (statements, EXEC blocks)
//! - Cols 73-80: identification area (ignored)
//!
//! We extract: programs, divisions, sections, paragraphs, EXEC SQL/CICS blocks,
//! ENTRY points, MOVE statements, file declarations, and JCL jobs/steps.

use serde::{Serialize, Deserialize};

// ── COBOL types ──────────────────────────────────────────────────────────────

/// A COBOL division.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CobolDivision {
    pub name: String,
    pub line: usize,
}

/// A COBOL section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CobolSection {
    pub name: String,
    pub division: String,
    pub line: usize,
}

/// A COBOL paragraph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CobolParagraph {
    pub name: String,
    pub section: Option<String>,
    pub line: usize,
}

/// An enriched COBOL block (EXEC SQL, EXEC CICS, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CobolBlock {
    pub kind: String,
    pub line: usize,
}

/// A JCL job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JclJob {
    pub name: String,
    pub line: usize,
}

/// A JCL step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JclStep {
    pub name: String,
    pub job_name: String,
    pub line: usize,
}

/// Result of COBOL/JCL processing.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CobolResult {
    pub programs: usize,
    pub divisions: usize,
    pub sections: usize,
    pub paragraphs: usize,
    pub exec_sql_blocks: usize,
    pub exec_cics_blocks: usize,
    pub entry_points: usize,
    pub moves: usize,
    pub file_declarations: usize,
    pub jcl_jobs: usize,
    pub jcl_steps: usize,
}

// ── Column helpers ───────────────────────────────────────────────────────────

/// Extract the source area of a COBOL line (cols 7-72, 0-indexed: 6..72).
/// Returns the trimmed content for keyword matching.
/// If the line is a comment (col 7 is `*` or `/`), returns None.
fn cobol_source_area(line: &str) -> Option<&str> {
    // Need at least 7 characters to have a source area
    let bytes = line.as_bytes();
    if bytes.len() <= 6 {
        return None;
    }
    // Check indicator area (col 7 = byte index 6)
    let indicator = bytes[6];
    // Comment lines: * or / in indicator area
    if indicator == b'*' || indicator == b'/' || indicator == b'-' {
        return None;
    }
    // Extract cols 8-72 (byte index 7..72), or to end of line if shorter
    let end = bytes.len().min(72);
    let area = &line[7..end];
    let trimmed = area.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed)
}

/// Check if a trimmed source-area string starts with a keyword (case-insensitive).
fn starts_with_keyword(haystack: &str, keyword: &str) -> bool {
    let h = haystack.as_bytes();
    let k = keyword.as_bytes();
    if h.len() < k.len() {
        return false;
    }
    h[..k.len()].eq_ignore_ascii_case(k)
        && h.get(k.len()).map(|c| !c.is_ascii_alphanumeric() && *c != b'-').unwrap_or(true)
}

/// Check if a trimmed source-area string contains a keyword (case-insensitive).
fn contains_keyword(haystack: &str, keyword: &str) -> bool {
    let hay_upper = haystack.to_ascii_uppercase();
    let key_upper = keyword.to_ascii_uppercase();
    hay_upper.contains(&key_upper)
}

/// Parse a name from the start of a source area: word characters and hyphens.
fn parse_name(s: &str) -> Option<&str> {
    let end = s.find(|c: char| !c.is_ascii_alphanumeric() && c != '-').unwrap_or(s.len());
    if end == 0 { None } else { Some(&s[..end]) }
}

// ── File classification ──────────────────────────────────────────────────────

/// Check if a file is a COBOL source file.
pub fn is_cobol_file(path: &str) -> bool {
    matches!(path.rsplit('.').next(), Some("cbl" | "cob" | "cpy" | "COB" | "CBL"))
}

/// Check if a file is a JCL file.
pub fn is_jcl_file(path: &str) -> bool {
    matches!(path.rsplit('.').next(), Some("jcl" | "JCL" | "prc" | "PRC"))
}

// ── COBOL extraction ─────────────────────────────────────────────────────────

/// Process a COBOL file and extract structure using column-aware scanning.
pub fn process_cobol_file(_path: &str, content: &str) -> CobolResult {
    let mut result = CobolResult::default();

    for line in content.lines() {
        let Some(area) = cobol_source_area(line) else { continue };

        // Division: "<NAME> DIVISION."
        if let Some(name) = parse_name(area) {
            let rest = &area[name.len()..].trim_start();
            if starts_with_keyword(rest, "DIVISION") && rest.as_bytes().get(8) == Some(&b'.') {
                result.divisions += 1;
                continue;
            }
        }

        // Section: "<NAME> SECTION."
        if let Some(name) = parse_name(area) {
            let rest = &area[name.len()..].trim_start();
            if starts_with_keyword(rest, "SECTION") && rest.as_bytes().get(7) == Some(&b'.') {
                result.sections += 1;
                continue;
            }
        }

        // Paragraph: "<NAME>." at margin A (starts in source area)
        if let Some(name) = parse_name(area) {
            let rest = &area[name.len()..];
            if rest.starts_with('.') {
                let upper = name.to_ascii_uppercase();
                if !upper.ends_with("DIVISION") && !upper.ends_with("SECTION") {
                    result.paragraphs += 1;
                }
                continue;
            }
        }

        // EXEC SQL (can appear in margin B, not necessarily at margin A)
        if contains_keyword(area, "EXEC SQL") {
            result.exec_sql_blocks += 1;
            continue;
        }

        // EXEC CICS
        if contains_keyword(area, "EXEC CICS") {
            result.exec_cics_blocks += 1;
            continue;
        }

        // ENTRY POINT
        if starts_with_keyword(area, "ENTRY") {
            let rest = &area["ENTRY".len()..].trim_start();
            if rest.starts_with('\'') || rest.starts_with('"') {
                result.entry_points += 1;
            }
        }

        // MOVE statement
        if starts_with_keyword(area, "MOVE") {
            result.moves += 1;
        }

        // FILE DECLARATION: "SELECT <name> ASSIGN TO"
        if starts_with_keyword(area, "SELECT") {
            let upper = area.to_ascii_uppercase();
            if upper.contains("ASSIGN TO") {
                result.file_declarations += 1;
            }
        }
    }

    if result.divisions > 0 || result.paragraphs > 0 {
        result.programs += 1;
    }

    result
}

// ── JCL extraction ───────────────────────────────────────────────────────────

/// Process a JCL file and extract job/step structure.
pub fn process_jcl_file(_path: &str, content: &str) -> CobolResult {
    let mut result = CobolResult::default();

    for line in content.lines() {
        let trimmed = line.trim();

        // JOB: "//<NAME> JOB ..."
        if trimmed.len() > 2 && trimmed.starts_with("//") {
            let rest = &trimmed[2..];
            if let Some(name) = parse_name(rest) {
                let after_name = &rest[name.len()..].trim_start();
                if starts_with_keyword(after_name, "JOB") {
                    result.jcl_jobs += 1;
                    continue;
                }
                if starts_with_keyword(after_name, "EXEC") {
                    result.jcl_steps += 1;
                    continue;
                }
            }
        }
    }

    result
}

// ── Batch processing ─────────────────────────────────────────────────────────

/// Process multiple COBOL/JCL files.
pub fn process_cobol_files(files: &[(String, String)]) -> CobolResult {
    let mut combined = CobolResult::default();
    for (path, content) in files {
        let result = if is_jcl_file(path) {
            process_jcl_file(path, content)
        } else {
            process_cobol_file(path, content)
        };
        combined.programs += result.programs;
        combined.divisions += result.divisions;
        combined.sections += result.sections;
        combined.paragraphs += result.paragraphs;
        combined.exec_sql_blocks += result.exec_sql_blocks;
        combined.exec_cics_blocks += result.exec_cics_blocks;
        combined.entry_points += result.entry_points;
        combined.moves += result.moves;
        combined.file_declarations += result.file_declarations;
        combined.jcl_jobs += result.jcl_jobs;
        combined.jcl_steps += result.jcl_steps;
    }
    combined
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_cobol_file() {
        assert!(is_cobol_file("src/program.cbl"));
        assert!(is_cobol_file("src/program.cob"));
        assert!(is_cobol_file("src/copybook.cpy"));
        assert!(!is_cobol_file("src/program.rs"));
    }

    #[test]
    fn test_is_jcl_file() {
        assert!(is_jcl_file("jobs/run.jcl"));
        assert!(is_jcl_file("jobs/run.JCL"));
        assert!(!is_jcl_file("jobs/run.sh"));
    }

    #[test]
    fn test_division_extraction() {
        let content = "       IDENTIFICATION DIVISION.\n       PROGRAM-ID. HELLO.\n       DATA DIVISION.\n       WORKING-STORAGE SECTION.\n       PROCEDURE DIVISION.\n           DISPLAY 'Hello'.\n           STOP RUN.\n";
        let result = process_cobol_file("test.cbl", content);
        assert_eq!(result.divisions, 3);
        assert_eq!(result.programs, 1);
    }

    #[test]
    fn test_section_extraction() {
        let content = "       DATA DIVISION.\n       WORKING-STORAGE SECTION.\n       LINKAGE SECTION.\n       PROCEDURE DIVISION.\n           MAIN-PARA.\n           DISPLAY 'Hello'.\n";
        let result = process_cobol_file("test.cbl", content);
        assert_eq!(result.sections, 2);
        assert!(result.paragraphs >= 1);
    }

    #[test]
    fn test_exec_sql() {
        let content = "       PROCEDURE DIVISION.\n           EXEC SQL\n              SELECT * FROM users\n           END-EXEC.\n           EXEC SQL\n              INSERT INTO logs VALUES (1)\n           END-EXEC.\n";
        let result = process_cobol_file("test.cbl", content);
        assert_eq!(result.exec_sql_blocks, 2);
    }

    #[test]
    fn test_exec_cics() {
        let content = "       PROCEDURE DIVISION.\n           EXEC CICS\n              SEND TEXT FROM('Hello')\n           END-EXEC.\n";
        let result = process_cobol_file("test.cbl", content);
        assert_eq!(result.exec_cics_blocks, 1);
    }

    #[test]
    fn test_jcl_job() {
        let content = "//MYJOB   JOB (ACCT),'DESCRIPTION'\n//STEP1   EXEC PGM=IEFBR14\n//STEP2   EXEC PGM=IEFBR14\n";
        let result = process_jcl_file("run.jcl", content);
        assert_eq!(result.jcl_jobs, 1);
        assert_eq!(result.jcl_steps, 2);
    }

    #[test]
    fn test_jcl_no_trailing_space() {
        // JOB line without trailing space after keyword
        let content = "//JOB1 JOB\n//STEP1 EXEC PGM=IEFBR14\n";
        let result = process_jcl_file("run.jcl", content);
        assert_eq!(result.jcl_jobs, 1);
        assert_eq!(result.jcl_steps, 1);
    }

    #[test]
    fn test_process_multiple_files() {
        let files = vec![
            ("prog1.cbl".to_string(), "       IDENTIFICATION DIVISION.\n       PROCEDURE DIVISION.\n           MAIN-PARA.\n".to_string()),
            ("run.jcl".to_string(), "//JOB1 JOB\n//STEP1 EXEC PGM=IEFBR14\n".to_string()),
        ];
        let result = process_cobol_files(&files);
        assert_eq!(result.programs, 1);
        assert_eq!(result.jcl_jobs, 1);
        assert_eq!(result.jcl_steps, 1);
    }

    #[test]
    fn test_comment_lines_skipped() {
        let content = "      * This is a comment\n       IDENTIFICATION DIVISION.\n       PROCEDURE DIVISION.\n";
        let result = process_cobol_file("test.cbl", content);
        assert_eq!(result.divisions, 2);
    }

    #[test]
    fn test_move_statement() {
        let content = "       PROCEDURE DIVISION.\n           MOVE WS-COUNT TO WS-TOTAL.\n";
        let result = process_cobol_file("test.cbl", content);
        assert_eq!(result.moves, 1);
    }

    #[test]
    fn test_file_declaration() {
        let content = "       ENVIRONMENT DIVISION.\n           INPUT-OUTPUT SECTION.\n           FILE-CONTROL.\n               SELECT MYFILE ASSIGN TO DISK.\n";
        let result = process_cobol_file("test.cbl", content);
        assert_eq!(result.file_declarations, 1);
    }

    #[test]
    fn test_entry_point() {
        let content = "       PROCEDURE DIVISION.\n           ENTRY 'MYENTRY'.\n";
        let result = process_cobol_file("test.cbl", content);
        assert_eq!(result.entry_points, 1);
    }

    #[test]
    fn test_cobol_source_area() {
        // Standard 7-space indent, col 7 is space (indicator)
        assert_eq!(cobol_source_area("       IDENTIFICATION DIVISION."), Some("IDENTIFICATION DIVISION."));
        // Comment line (col 7 = *)
        assert_eq!(cobol_source_area("      * This is a comment"), None);
        // Short line
        assert_eq!(cobol_source_area("short"), None);
        // Empty source area
        assert_eq!(cobol_source_area("       "), None);
    }

    #[test]
    fn test_starts_with_keyword() {
        assert!(starts_with_keyword("DIVISION.", "DIVISION"));
        assert!(starts_with_keyword("DIVISION foo", "DIVISION"));
        assert!(!starts_with_keyword("DIVISIONS foo", "DIVISION"));
        assert!(starts_with_keyword("SECTION.", "SECTION"));
        assert!(starts_with_keyword("SECTION foo", "SECTION"));
        assert!(!starts_with_keyword("SECTIONAL foo", "SECTION"));
        assert!(starts_with_keyword("EXEC SQL", "EXEC"));
    }
}
