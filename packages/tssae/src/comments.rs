use std::collections::BTreeSet;

use serde::Serialize;
use tsrs_ast::{Kind, Node, SourceFile};
use tsrs_core::P;
use tsrs_scanner::{
    get_ecma_line_and_utf16_character_of_position, get_ecma_line_starts, get_token_pos_of_node,
};

use crate::projection::SourceLocation;
use crate::projects::{is_project_source_file, relative_to_cwd};
use crate::serve::SnapshotState;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CommentKind {
    Line,
    Block,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScannedComment {
    pub kind: CommentKind,
    pub text: String,
    pub at: SourceLocation,
    pub next_code_line: Option<u32>,
    pub is_jsx: bool,
}

#[derive(Debug, Clone, Copy)]
struct OpaqueSpan {
    start: u32,
    end: u32,
    is_code: bool,
}

#[derive(Debug, Clone, Copy)]
struct RawComment {
    start: u32,
    end: u32,
    is_line: bool,
    is_jsdoc: bool,
}

/// Scans project source files in the active snapshot for standalone line and block comments,
/// returning each comment's normalized text, exact span, and immediately following code line.
pub fn scan_snapshot_comments(
    snapshot: &SnapshotState,
    files: Option<&[String]>,
) -> Vec<ScannedComment> {
    let filter_set = files.map(|list| {
        list.iter()
            .map(|f| {
                let norm = f.replace('\\', "/");
                norm.trim_start_matches("./").to_string()
            })
            .collect::<BTreeSet<String>>()
    });
    if filter_set.as_ref().is_some_and(BTreeSet::is_empty) {
        return Vec::new();
    }

    let mut seen_files = BTreeSet::new();
    let mut comments = Vec::new();

    for project_id in snapshot.project_ids.values() {
        let Ok(program) = snapshot.snapshot_data.get_program(project_id) else {
            continue;
        };
        for &sf in program.get_source_files() {
            if !is_project_source_file(program, sf) {
                continue;
            }
            let rel_file = relative_to_cwd(&snapshot.cwd, sf.file_name());
            if let Some(allowed) = &filter_set
                && !allowed.contains(&rel_file)
            {
                continue;
            }
            if !seen_files.insert(rel_file.clone()) {
                continue;
            }
            comments.extend(scan_source_file_comments(sf, &rel_file));
        }
    }

    comments.sort_by(|a, b| {
        a.at.file
            .cmp(&b.at.file)
            .then_with(|| a.at.start.cmp(&b.at.start))
            .then_with(|| a.at.end.cmp(&b.at.end))
    });
    comments
}

pub fn scan_source_file_comments(sf: P<SourceFile>, rel_file: &str) -> Vec<ScannedComment> {
    let text = sf.text();
    let len = u32::try_from(text.len()).unwrap_or(u32::MAX);
    let line_starts = get_ecma_line_starts(sf.get());
    let num_lines = line_starts.len().max(1);

    let mut opaque_spans = Vec::new();
    let mut jsx_wrappers = Vec::new();
    collect_ast_spans(sf.as_node(), sf, len, &mut opaque_spans, &mut jsx_wrappers);
    opaque_spans.sort_by_key(|span| (span.start, span.end));
    jsx_wrappers.sort_by_key(|span| (span.0, span.1));

    let mut line_has_code = vec![false; num_lines];
    let mut line_has_comment = vec![false; num_lines];
    let comments = scan_tokens_and_comments(
        text,
        line_starts,
        &opaque_spans,
        &jsx_wrappers,
        &mut line_has_code,
        &mut line_has_comment,
    );

    let mut results = Vec::new();
    for comment in comments {
        if comment.is_jsdoc {
            continue;
        }
        let Some((eff_start, eff_end, is_jsx)) =
            standalone_span(text, line_starts, &comment, &jsx_wrappers)
        else {
            continue;
        };
        let Some(body) = extract_comment_body(text, &comment) else {
            continue;
        };

        let at = span_location(sf, rel_file, eff_start, eff_end);
        let end_pos = eff_end.saturating_sub(1).max(eff_start);
        let end_line = byte_to_line1(line_starts, end_pos);
        let next_code_line = find_next_code_line(end_line, &line_has_code, &line_has_comment);

        results.push(ScannedComment {
            kind: if comment.is_line {
                CommentKind::Line
            } else {
                CommentKind::Block
            },
            text: body,
            at,
            next_code_line,
            is_jsx,
        });
    }

    results
}

fn collect_ast_spans(
    node: P<Node>,
    sf: P<SourceFile>,
    text_len: u32,
    opaque_spans: &mut Vec<OpaqueSpan>,
    jsx_wrappers: &mut Vec<(u32, u32)>,
) {
    match node.kind() {
        Kind::StringLiteral
        | Kind::NoSubstitutionTemplateLiteral
        | Kind::TemplateHead
        | Kind::TemplateMiddle
        | Kind::TemplateTail
        | Kind::RegularExpressionLiteral => {
            let start = get_token_pos_of_node(node, sf, false).min(text_len);
            let end = node
                .end()
                .source_offset()
                .unwrap_or(start)
                .clamp(start, text_len);
            if end > start {
                opaque_spans.push(OpaqueSpan {
                    start,
                    end,
                    is_code: true,
                });
            }
            return;
        }
        Kind::JsxText | Kind::JsxTextAllWhiteSpaces => {
            let start = get_token_pos_of_node(node, sf, false).min(text_len);
            let end = node
                .end()
                .source_offset()
                .unwrap_or(start)
                .clamp(start, text_len);
            if end > start {
                let is_trivia_ws = node.kind() == Kind::JsxTextAllWhiteSpaces
                    || node.as_jsx_text().contains_only_trivia_white_spaces;
                opaque_spans.push(OpaqueSpan {
                    start,
                    end,
                    is_code: !is_trivia_ws,
                });
            }
            return;
        }
        Kind::JsxExpression if node.as_jsx_expression().expression.is_none() => {
            let start = get_token_pos_of_node(node, sf, false).min(text_len);
            let end = node
                .end()
                .source_offset()
                .unwrap_or(start)
                .clamp(start, text_len);
            if end >= start + 2 {
                let bytes = sf.text().as_bytes();
                if bytes.get(usize::try_from(start).unwrap_or(0)) == Some(&b'{')
                    && bytes.get(usize::try_from(end - 1).unwrap_or(0)) == Some(&b'}')
                {
                    jsx_wrappers.push((start, end));
                }
            }
        }
        _ => {}
    }

    node.for_each_child(&mut |child| {
        collect_ast_spans(child, sf, text_len, opaque_spans, jsx_wrappers);
        false
    });
}

fn scan_tokens_and_comments(
    text: &str,
    line_starts: &[u32],
    opaque_spans: &[OpaqueSpan],
    jsx_wrappers: &[(u32, u32)],
    line_has_code: &mut [bool],
    line_has_comment: &mut [bool],
) -> Vec<RawComment> {
    let bytes = text.as_bytes();
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    let mut pos: u32 = 0;
    let mut opaque_idx = 0usize;
    let mut jsx_idx = 0usize;
    let mut comments = Vec::new();

    if text.starts_with("#!") {
        while let Some(&b) = bytes.get(usize::try_from(pos).unwrap_or(usize::MAX)) {
            if b == b'\n' || b == b'\r' {
                break;
            }
            pos += 1;
        }
        if let Some(first) = line_has_comment.first_mut() {
            *first = true;
        }
    }

    while pos < len {
        while opaque_idx < opaque_spans.len() && opaque_spans[opaque_idx].end <= pos {
            opaque_idx += 1;
        }
        if let Some(span) = opaque_spans.get(opaque_idx)
            && pos >= span.start
            && pos < span.end
        {
            if span.is_code {
                mark_span_lines(line_starts, span.start, span.end, line_has_code);
            }
            pos = span.end;
            continue;
        }

        while jsx_idx < jsx_wrappers.len() && jsx_wrappers[jsx_idx].1 <= pos {
            jsx_idx += 1;
        }
        if let Some(&(j_start, j_end)) = jsx_wrappers.get(jsx_idx)
            && (pos == j_start || pos + 1 == j_end)
        {
            pos += 1;
            continue;
        }

        let pos_usize = usize::try_from(pos).unwrap_or(bytes.len());
        let Some(rest) = text.get(pos_usize..) else {
            break;
        };
        let Some(ch) = rest.chars().next() else {
            break;
        };

        if ch.is_whitespace() {
            pos += u32::try_from(ch.len_utf8()).unwrap_or(1);
            continue;
        }

        if rest.starts_with("//") {
            let start = pos;
            pos += 2;
            while let Some(slice) = text.get(usize::try_from(pos).unwrap_or(bytes.len())..) {
                let Some(next_ch) = slice.chars().next() else {
                    break;
                };
                if is_ecma_line_break(next_ch) {
                    break;
                }
                pos += u32::try_from(next_ch.len_utf8()).unwrap_or(1);
            }
            mark_span_lines(line_starts, start, pos.max(start + 1), line_has_comment);
            comments.push(RawComment {
                start,
                end: pos,
                is_line: true,
                is_jsdoc: false,
            });
            continue;
        }

        if rest.starts_with("/*") {
            let start = pos;
            let body_start = usize::try_from(pos + 2).unwrap_or(bytes.len());
            let end = bytes
                .get(body_start..)
                .and_then(|tail| tail.windows(2).position(|w| w == b"*/"))
                .map_or(len, |rel| pos + 2 + u32::try_from(rel).unwrap_or(0) + 2);
            let raw = text
                .get(usize::try_from(start).unwrap_or(0)..usize::try_from(end).unwrap_or(0))
                .unwrap_or("");
            let is_jsdoc = raw.starts_with("/**") && raw != "/**/";
            mark_span_lines(line_starts, start, end, line_has_comment);
            comments.push(RawComment {
                start,
                end,
                is_line: false,
                is_jsdoc,
            });
            pos = end;
            continue;
        }

        let char_len = u32::try_from(ch.len_utf8()).unwrap_or(1);
        mark_span_lines(line_starts, pos, pos + char_len, line_has_code);
        pos += char_len;
    }

    comments
}

fn mark_span_lines(line_starts: &[u32], start: u32, end: u32, flags: &mut [bool]) {
    if end <= start || flags.is_empty() {
        return;
    }
    let first_line = byte_to_line1(line_starts, start);
    let last_line = byte_to_line1(line_starts, end.saturating_sub(1).max(start));
    let first_idx = usize::try_from(first_line.saturating_sub(1)).unwrap_or(0);
    let last_idx = usize::try_from(last_line.saturating_sub(1))
        .unwrap_or(0)
        .min(flags.len().saturating_sub(1));
    for flag in &mut flags[first_idx..=last_idx] {
        *flag = true;
    }
}

fn standalone_span(
    text: &str,
    line_starts: &[u32],
    comment: &RawComment,
    jsx_wrappers: &[(u32, u32)],
) -> Option<(u32, u32, bool)> {
    let (eff_start, eff_end, is_jsx) = if let Some(&(j_start, j_end)) = jsx_wrappers
        .iter()
        .find(|&&(js, je)| comment.start > js && comment.end < je)
    {
        let before = text
            .get(
                usize::try_from(j_start + 1).unwrap_or(0)
                    ..usize::try_from(comment.start).unwrap_or(0),
            )
            .unwrap_or("");
        let after = text
            .get(usize::try_from(comment.end).unwrap_or(0)..usize::try_from(j_end - 1).unwrap_or(0))
            .unwrap_or("");
        if !before.chars().all(char::is_whitespace) || !after.chars().all(char::is_whitespace) {
            return None;
        }
        (j_start, j_end, true)
    } else {
        (comment.start, comment.end, false)
    };

    let start_line_idx = usize::try_from(byte_to_line1(line_starts, eff_start) - 1).unwrap_or(0);
    let line_start = line_starts.get(start_line_idx).copied().unwrap_or(0);
    let prefix = text
        .get(usize::try_from(line_start).unwrap_or(0)..usize::try_from(eff_start).unwrap_or(0))
        .unwrap_or("");
    if !prefix.chars().all(char::is_whitespace) {
        return None;
    }

    let end_pos = eff_end.saturating_sub(1).max(eff_start);
    let end_line_idx = usize::try_from(byte_to_line1(line_starts, end_pos) - 1).unwrap_or(0);
    let next_line_start = line_starts
        .get(end_line_idx + 1)
        .copied()
        .unwrap_or_else(|| u32::try_from(text.len()).unwrap_or(u32::MAX));
    let suffix = text
        .get(
            usize::try_from(eff_end).unwrap_or(0)
                ..usize::try_from(next_line_start).unwrap_or(text.len()),
        )
        .unwrap_or("");
    if !suffix.chars().all(char::is_whitespace) {
        return None;
    }

    Some((eff_start, eff_end, is_jsx))
}

fn extract_comment_body(text: &str, comment: &RawComment) -> Option<String> {
    let raw = text.get(
        usize::try_from(comment.start).unwrap_or(0)..usize::try_from(comment.end).unwrap_or(0),
    )?;
    if comment.is_line {
        return Some(raw.strip_prefix("//")?.trim().to_string());
    }

    let inner = raw.strip_prefix("/*")?;
    let inner = inner.strip_suffix("*/").unwrap_or(inner);
    let mut lines = Vec::new();
    for (idx, line) in split_ecma_lines(inner).enumerate() {
        let trimmed = if idx == 0 {
            line.trim()
        } else {
            let ws_trimmed = line.trim_start();
            ws_trimmed
                .strip_prefix('*')
                .map_or(ws_trimmed, str::trim)
                .trim()
        };
        if !trimmed.is_empty() {
            lines.push(trimmed);
        }
    }
    Some(lines.join(" "))
}

fn find_next_code_line(
    comment_end_line: u32,
    line_has_code: &[bool],
    line_has_comment: &[bool],
) -> Option<u32> {
    let start_idx = usize::try_from(comment_end_line).unwrap_or(usize::MAX);
    for idx in start_idx..line_has_code.len() {
        if line_has_code[idx] {
            return Some(u32::try_from(idx + 1).unwrap_or(u32::MAX));
        }
        if !line_has_comment[idx] {
            return None;
        }
    }
    None
}

fn byte_to_line1(line_starts: &[u32], byte_pos: u32) -> u32 {
    let idx = line_starts.partition_point(|&start| start <= byte_pos);
    u32::try_from(idx.max(1)).unwrap_or(u32::MAX)
}

fn span_location(sf: P<SourceFile>, rel_file: &str, start: u32, end: u32) -> SourceLocation {
    let (start_line0, start_col0) = get_ecma_line_and_utf16_character_of_position(sf.get(), start);
    let (end_line0, end_col0) = get_ecma_line_and_utf16_character_of_position(sf.get(), end);
    SourceLocation {
        file: rel_file.to_string(),
        line: u32::try_from(start_line0).unwrap_or(u32::MAX - 1) + 1,
        column: start_col0 + 1,
        end_line: u32::try_from(end_line0).unwrap_or(u32::MAX - 1) + 1,
        end_column: end_col0 + 1,
        start,
        end,
    }
}

const fn is_ecma_line_break(ch: char) -> bool {
    matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn split_ecma_lines(text: &str) -> impl Iterator<Item = &str> {
    text.split(['\n', '\u{2028}', '\u{2029}'])
        .flat_map(|segment| segment.split('\r'))
}
