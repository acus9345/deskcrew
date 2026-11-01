// What an agent reads: one short line per control, and only what changed after that.
use std::collections::BTreeMap;

pub type Seen = BTreeMap<u32, String>;

/// Most change lines one reply carries before pointing at `see`.
pub const MAX_CHANGES: usize = 30;
pub const MAX_LINES: usize = 120;
const MAX_NAME: usize = 80;
const MAX_VALUE: usize = 60;
const MAX_CELL: usize = 32;
const MAX_CELLS: usize = 6;

/// One line of text, cut to length, so a long value cannot flood a reply.
pub fn clip(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max).collect();
    format!("{cut}…")
}

/// UIA control types, by the numbers Windows gives them.
pub fn role(kind: i32) -> Option<&'static str> {
    Some(match kind {
        50000 | 50031 => "button",
        50001 => "calendar",
        50002 => "checkbox",
        50003 => "combobox",
        50004 => "edit",
        50005 => "link",
        50006 => "image",
        50007 | 50024 => "item",
        50011 => "menuitem",
        50012 => "progress",
        50013 => "radio",
        50015 => "slider",
        50016 => "spinner",
        50019 => "tab",
        50020 => "",
        50029 => "row",
        50030 => "document",
        50035 => "header",
        _ => return None,
    })
}

/// Parts of a window that never help an agent, along with everything inside them.
pub fn skipped(kind: i32) -> bool {
    matches!(kind, 50014 | 50022 | 50027 | 50037 | 50038)
}

/// The buttons every window has. Store apps list them outside any title bar.
pub fn is_window_chrome(automation_id: &str) -> bool {
    matches!(automation_id, "Minimize" | "Maximize" | "Restore" | "Close" | "TitleBar" | "SystemMenuBar" | "SystemMenuBarItem" | "view_0" | "view_1")
}

/// Rows read as one line: their plain text joins the row instead of adding lines.
pub fn is_row(kind: i32) -> bool {
    matches!(kind, 50007 | 50024 | 50029)
}

/// A role for a container only worth listing because it can be acted on.
pub fn container_role(kind: i32) -> &'static str {
    match kind {
        50008 => "list",
        50023 => "tree",
        50028 | 50036 => "table",
        50009 | 50010 => "menu",
        50018 => "tabs",
        50021 => "toolbar",
        50033 => "pane",
        _ => "group",
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Parts {
    pub role: String,
    pub name: String,
    pub value: Option<String>,
    pub states: Vec<&'static str>,
    pub cells: Vec<String>,
}

pub fn line(parts: &Parts) -> String {
    let mut out = parts.role.clone();
    if !parts.name.is_empty() {
        if !out.is_empty() {
            out.push(' ');
        }
        if parts.name.starts_with('#') {
            out.push_str(&parts.name);
        } else {
            out.push('"');
            out.push_str(&clip(&parts.name, MAX_NAME));
            out.push('"');
        }
    }
    if let Some(value) = &parts.value {
        out.push_str(" =");
        out.push_str(&clip(value, MAX_VALUE));
    }
    for state in &parts.states {
        out.push_str(" [");
        out.push_str(state);
        out.push(']');
    }
    for cell in parts.cells.iter().take(MAX_CELLS) {
        out.push_str(" | ");
        out.push_str(&clip(cell, MAX_CELL));
    }
    out
}

fn runs(refs: &[u32]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut at = 0;
    while at < refs.len() {
        let mut end = at;
        while end + 1 < refs.len() && refs[end + 1] == refs[end] + 1 {
            end += 1;
        }
        out.push(if end == at { format!("e{}", refs[at]) } else { format!("e{}-e{}", refs[at], refs[end]) });
        at = end + 1;
    }
    out.join(" ")
}

/// New and changed controls as whole lines, then everything gone on one line.
pub fn changes(old: &Seen, new: &Seen) -> Vec<String> {
    let mut out = Vec::new();
    for (id, text) in new {
        match old.get(id) {
            None => out.push(format!("+ e{id} {text}")),
            Some(before) if before != text => out.push(format!("~ e{id} {text}")),
            _ => {}
        }
    }
    let gone: Vec<u32> = old.keys().filter(|id| !new.contains_key(id)).copied().collect();
    if !gone.is_empty() {
        out.push(format!("- {}", runs(&gone)));
    }
    out
}

/// Changes as a reply, capped. False when some were left out, so `see` still owes them.
pub fn change_report(old: &Seen, new: &Seen) -> (Vec<String>, bool) {
    let mut lines = changes(old, new);
    if lines.len() <= MAX_CHANGES {
        return (lines, true);
    }
    let left = lines.len() - MAX_CHANGES;
    lines.truncate(MAX_CHANGES);
    lines.push(format!("… {left} more changes: see"));
    (lines, false)
}

/// One screenful of lines from `from`, with the way to the rest.
pub fn page(seen: &Seen, find: Option<&str>, from: usize) -> Vec<String> {
    let needle = find.map(str::to_lowercase);
    let matched: Vec<String> = seen
        .iter()
        .filter(|(_, text)| needle.as_ref().is_none_or(|n| text.to_lowercase().contains(n)))
        .map(|(id, text)| format!("e{id} {text}"))
        .collect();
    let total = matched.len();
    let mut out: Vec<String> = matched.into_iter().skip(from).take(MAX_LINES).collect();
    if from + out.len() < total {
        out.push(format!("… {} more: see --from {} or --find word", total - from - out.len(), from + MAX_LINES));
    }
    out
}

/// A command line's pieces, honouring quotes, for the steps of `do`.
pub fn split_steps(script: &str) -> Vec<Vec<String>> {
    let mut steps = Vec::new();
    let mut step = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut started = false;
    for c in script.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => word.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                started = true;
            }
            None if c == ';' || c == '\n' || c.is_whitespace() => {
                if started {
                    step.push(std::mem::take(&mut word));
                    started = false;
                }
                if (c == ';' || c == '\n') && !step.is_empty() {
                    steps.push(std::mem::take(&mut step));
                }
            }
            None => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        step.push(word);
    }
    if !step.is_empty() {
        steps.push(step);
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(items: &[(u32, &str)]) -> Seen {
        items.iter().map(|(id, text)| (*id, text.to_string())).collect()
    }

    #[test]
    fn a_line_says_only_what_is_there() {
        let button = Parts { role: "button".into(), name: "Save".into(), ..Parts::default() };
        assert_eq!(line(&button), r#"button "Save""#);
        let text = Parts { name: "Ready".into(), ..Parts::default() };
        assert_eq!(line(&text), r#""Ready""#);
        let unnamed = Parts { role: "button".into(), name: "#closeBtn".into(), ..Parts::default() };
        assert_eq!(line(&unnamed), "button #closeBtn");
        let check = Parts { role: "checkbox".into(), name: "Wrap".into(), states: vec!["on"], ..Parts::default() };
        assert_eq!(line(&check), r#"checkbox "Wrap" [on]"#);
        let row = Parts { role: "item".into(), name: "a.txt".into(), cells: vec!["1 KB".into(), "Text".into()], ..Parts::default() };
        assert_eq!(line(&row), r#"item "a.txt" | 1 KB | Text"#);
        let field = Parts { role: "edit".into(), name: "Name".into(), value: Some("Ann".into()), ..Parts::default() };
        assert_eq!(line(&field), r#"edit "Name" =Ann"#);
    }

    #[test]
    fn long_text_is_cut_and_flattened() {
        assert_eq!(clip("a\n  b", 10), "a b");
        assert_eq!(clip("abcdef", 3), "abc…");
        assert_eq!(clip("字元對應表", 2), "字元…");
    }

    #[test]
    fn scrollbars_and_title_bars_are_left_out() {
        for kind in [50014, 50022, 50027, 50037, 50038] {
            assert!(skipped(kind));
        }
        assert!(!skipped(50000));
    }

    #[test]
    fn changes_list_additions_edits_and_removals() {
        let old = seen(&[(1, "a"), (2, "b"), (3, "c"), (4, "d"), (7, "x")]);
        let new = seen(&[(1, "a"), (2, "B"), (9, "new")]);
        assert_eq!(changes(&old, &new), ["~ e2 B", "+ e9 new", "- e3-e4 e7"]);
    }

    #[test]
    fn no_change_is_empty() {
        let same = seen(&[(1, "a")]);
        assert!(changes(&same, &same).is_empty());
    }

    #[test]
    fn a_flood_of_changes_is_capped_and_says_so() {
        let new: Seen = (0..50).map(|i| (i, "x".to_string())).collect();
        let (lines, complete) = change_report(&Seen::new(), &new);
        assert_eq!(lines.len(), MAX_CHANGES + 1);
        assert!(!complete);
        assert!(lines.last().unwrap().contains("20 more"));
    }

    #[test]
    fn pages_filter_and_point_at_the_rest() {
        let all: Seen = (0..200).map(|i| (i, format!("row {i}"))).collect();
        let first = page(&all, None, 0);
        assert_eq!(first.len(), MAX_LINES + 1);
        assert!(first.last().unwrap().contains("--from 120"));
        assert_eq!(page(&all, Some("ROW 199"), 0), ["e199 row 199"]);
        assert_eq!(page(&all, None, 180).len(), 20);
    }

    #[test]
    fn steps_split_on_semicolons_outside_quotes() {
        assert_eq!(
            split_steps(r#"click e1; set e2 "a; b" ; key enter"#),
            vec![vec!["click", "e1"], vec!["set", "e2", "a; b"], vec!["key", "enter"]]
        );
        assert_eq!(split_steps("set e1 ''"), vec![vec!["set", "e1", ""]]);
    }
}
