use std::collections::BTreeSet;

pub use tssae_tsrs_boundary::ScannedComment;
use tssae_tsrs_boundary::SyntaxView;

use crate::serve::SnapshotState;

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

    let syntax_view = SyntaxView::new(&snapshot.cwd);
    let mut seen_files = BTreeSet::new();
    let mut comments = Vec::new();

    for project_id in snapshot.project_ids.values() {
        let Ok(program) = snapshot.snapshot_data.get_program(project_id) else {
            continue;
        };
        syntax_view.scan_program_comments(
            program,
            filter_set.as_ref(),
            &mut seen_files,
            &mut comments,
        );
    }

    comments.sort_by(|a, b| {
        a.at.file
            .cmp(&b.at.file)
            .then_with(|| a.at.start.cmp(&b.at.start))
            .then_with(|| a.at.end.cmp(&b.at.end))
    });
    comments
}
