//! Versioned session-recovery record, split-tree reconstruction and restore layout.
//!
//! Shared with the WASI plugin: pure data and string building only (no filesystem,
//! processes or libc).

use std::collections::BTreeMap;
use std::fmt::Write;

use serde::{Deserialize, Serialize};

pub const RECOVERY_VERSION: u32 = 1;
pub const RECOVERY_REMOVAL_GRACE_MILLIS: u64 = 3_000;
pub const RECOVERY_COALESCE_MILLIS: u64 = 1_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryPane {
    pub cwd: Option<String>,
    pub name: String,
    pub renamed: bool,
    /// "claude" | "omp" | "codex" only.
    pub harness: Option<String>,
    pub agent_id: Option<String>,
    pub focused: bool,
}

/// A restored pane's recorded harness association, kept while its resume is refused or
/// fails so a later `bkpr resume` can retry it. The launcher seeds it by restore-layout
/// leaf key; the pane's `bkpr restore-pane` claims it for its terminal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreLink {
    pub harness: String,
    pub agent_id: String,
}

/// KDL `split_direction` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    Vertical,
    Horizontal,
}

impl SplitDirection {
    fn kdl(self) -> &'static str {
        match self {
            Self::Vertical => "vertical",
            Self::Horizontal => "horizontal",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SplitNode<P> {
    Pane {
        pane: P,
    },
    Split {
        direction: SplitDirection,
        children: Vec<SplitNode<P>>,
    },
    Stack {
        panes: Vec<P>,
    },
}

impl<P> SplitNode<P> {
    /// Leaves in depth-first order.
    pub fn leaves(&self) -> Vec<&P> {
        match self {
            Self::Pane { pane } => vec![pane],
            Self::Split { children, .. } => children.iter().flat_map(Self::leaves).collect(),
            Self::Stack { panes } => panes.iter().collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryTab {
    pub name: String,
    pub focused: bool,
    pub layout: SplitNode<RecoveryPane>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryWorkspace {
    pub name: String,
    pub tabs: Vec<RecoveryTab>,
}

/// What the owner plugin sends; the native helper adds the header fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryUpdate {
    pub session_name: String,
    /// The owner's Bkpr session id (`configuration["session_id"]`).
    pub session_id: String,
    pub sequence: u64,
    pub workspaces: Vec<RecoveryWorkspace>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryRecord {
    pub version: u32,
    pub session_name: String,
    pub session_id: String,
    pub sequence: u64,
    /// `env!("CARGO_PKG_VERSION")` of the writer.
    pub written_by: String,
    pub updated_at_millis: u64,
    pub workspaces: Vec<RecoveryWorkspace>,
}

#[derive(Debug)]
pub enum RecoveryDecodeError {
    Malformed(serde_json::Error),
    /// Written by a newer Bkpr; never reinterpret it.
    FutureVersion(u64),
}

impl std::fmt::Display for RecoveryDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(error) => write!(f, "malformed recovery record: {error}"),
            Self::FutureVersion(version) => write!(
                f,
                "recovery record version {version} is newer than supported {RECOVERY_VERSION}"
            ),
        }
    }
}

impl std::error::Error for RecoveryDecodeError {}

impl RecoveryRecord {
    pub fn decode(bytes: &[u8]) -> Result<Self, RecoveryDecodeError> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(RecoveryDecodeError::Malformed)?;
        // Check the version first: a future schema may not match this shape.
        if let Some(version) = value.get("version").and_then(serde_json::Value::as_u64)
            && version > u64::from(RECOVERY_VERSION)
        {
            return Err(RecoveryDecodeError::FutureVersion(version));
        }
        serde_json::from_value(value).map_err(RecoveryDecodeError::Malformed)
    }

    pub fn pane_count(&self) -> usize {
        self.workspaces
            .iter()
            .flat_map(|workspace| &workspace.tabs)
            .map(|tab| tab.layout.leaves().len())
            .sum()
    }

    pub fn usable(&self) -> bool {
        self.pane_count() > 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeometryPane {
    pub id: u32,
    pub x: usize,
    pub y: usize,
    pub columns: usize,
    pub rows: usize,
}

/// Reconstruct a split tree from tiled pane rectangles by recursive guillotine cuts.
/// Layouts that are not guillotine-cuttable degrade to a flat vertical split in id order.
///
/// Panes sharing one exact rectangle are a stack. The pinned runtime keeps one stack
/// member visible and parks the others as suppressed panes resized to its full rectangle;
/// tiled panes never overlap otherwise, so the cut runs on one representative per
/// rectangle and each shared rectangle becomes a stack in id order.
pub fn split_tree(panes: &[GeometryPane]) -> Option<SplitNode<u32>> {
    let mut members: BTreeMap<(usize, usize, usize, usize), Vec<u32>> = BTreeMap::new();
    for pane in panes {
        members
            .entry((pane.x, pane.y, pane.columns, pane.rows))
            .or_default()
            .push(pane.id);
    }
    let mut representatives = Vec::new();
    let mut stacks = BTreeMap::new();
    for ((x, y, columns, rows), mut ids) in members {
        ids.sort_unstable();
        representatives.push(GeometryPane {
            id: ids[0],
            x,
            y,
            columns,
            rows,
        });
        stacks.insert(ids[0], ids);
    }
    if representatives.is_empty() {
        return None;
    }
    Some(expand_shared(build_tree(&representatives), &stacks))
}

/// Replace each representative leaf with every pane sharing its rectangle.
fn expand_shared(node: SplitNode<u32>, stacks: &BTreeMap<u32, Vec<u32>>) -> SplitNode<u32> {
    let shared = |id: u32| stacks.get(&id).cloned().unwrap_or_else(|| vec![id]);
    match node {
        SplitNode::Pane { pane } => match shared(pane) {
            ids if ids.len() > 1 => SplitNode::Stack { panes: ids },
            _ => SplitNode::Pane { pane },
        },
        SplitNode::Stack { panes } => SplitNode::Stack {
            panes: panes.into_iter().flat_map(shared).collect(),
        },
        SplitNode::Split {
            direction,
            children,
        } => SplitNode::Split {
            direction,
            children: children
                .into_iter()
                .map(|child| expand_shared(child, stacks))
                .collect(),
        },
    }
}

fn build_tree(panes: &[GeometryPane]) -> SplitNode<u32> {
    if let [only] = panes {
        return SplitNode::Pane { pane: only.id };
    }
    if let Some(stack) = as_stack(panes) {
        return stack;
    }
    for direction in [SplitDirection::Vertical, SplitDirection::Horizontal] {
        let groups = cut(panes, direction);
        if groups.len() > 1 {
            return SplitNode::Split {
                direction,
                children: groups.iter().map(|group| build_tree(group)).collect(),
            };
        }
    }
    let mut by_id = panes.to_vec();
    by_id.sort_by_key(|pane| pane.id);
    SplitNode::Split {
        direction: SplitDirection::Vertical,
        children: by_id
            .into_iter()
            .map(|pane| SplitNode::Pane { pane: pane.id })
            .collect(),
    }
}

/// Partition at lines no pane straddles. Vertical cut lines separate columns.
fn cut(panes: &[GeometryPane], direction: SplitDirection) -> Vec<Vec<GeometryPane>> {
    let span = |pane: &GeometryPane| match direction {
        SplitDirection::Vertical => (pane.x, pane.x + pane.columns),
        SplitDirection::Horizontal => (pane.y, pane.y + pane.rows),
    };
    let mut sorted = panes.to_vec();
    match direction {
        SplitDirection::Vertical => sorted.sort_by_key(|pane| (pane.x, pane.y, pane.id)),
        SplitDirection::Horizontal => sorted.sort_by_key(|pane| (pane.y, pane.x, pane.id)),
    }
    let mut groups: Vec<Vec<GeometryPane>> = Vec::new();
    let mut group_end = 0;
    for pane in sorted {
        match groups.last_mut() {
            Some(group) if span(&pane).0 < group_end => {
                group_end = group_end.max(span(&pane).1);
                group.push(pane);
            }
            _ => {
                group_end = span(&pane).1;
                groups.push(vec![pane]);
            }
        }
    }
    groups
}

/// A single column of vertically contiguous panes where at most one is expanded: a
/// classic stack with 1-row collapsed members, as the runtime reports it briefly before
/// folding hidden members into suppressed panes.
fn as_stack(panes: &[GeometryPane]) -> Option<SplitNode<u32>> {
    let mut sorted = panes.to_vec();
    sorted.sort_by_key(|pane| (pane.y, pane.id));
    let first = sorted[0];
    let aligned = sorted
        .iter()
        .all(|pane| pane.x == first.x && pane.columns == first.columns);
    let contiguous = sorted
        .windows(2)
        .all(|pair| pair[1].y == pair[0].y + pair[0].rows);
    let collapsed = sorted.iter().filter(|pane| pane.rows == 1).count();
    (aligned && contiguous && collapsed + 1 >= sorted.len()).then(|| SplitNode::Stack {
        panes: sorted.iter().map(|pane| pane.id).collect(),
    })
}

/// One restore leaf: its KDL `command`/`args` plus its stable leaf key (`w{i}t{j}p{k}`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreLeaf {
    pub key: String,
    pub cwd: String,
    pub program: String,
    pub args: Vec<String>,
}

/// Restored workspace ids sort before any later-allocated `…:workspace:bkpr:{n}` id.
pub fn restored_workspace_id(session_id: &str, index: usize) -> String {
    format!("{session_id}:workspace:bkpr:0{index:04}")
}

/// Build a startup layout recreating every recorded workspace, tab and split tree.
/// Sizes are intentionally omitted except the sidebar's.
pub fn recovery_layout(
    plugin_url: &str,
    sidebar_configuration: &BTreeMap<String, String>,
    sidebar_percent: u8,
    session_id: &str,
    record: &RecoveryRecord,
    leaf: &mut dyn FnMut(&str, &RecoveryPane) -> RestoreLeaf,
) -> Result<String, serde_json::Error> {
    let quote = |value: &str| serde_json::to_string(value);
    let mut layout = crate::layout_prefix(plugin_url, sidebar_configuration, sidebar_percent)?;
    // Exactly one tab is focused: the first recorded one, else the first tab.
    let focused_tab = record
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.tabs)
        .position(|tab| tab.focused)
        .unwrap_or(0);
    let mut tab_index = 0;
    for (w, workspace) in record.workspaces.iter().enumerate() {
        let workspace_id = restored_workspace_id(session_id, w);
        for (t, tab) in workspace.tabs.iter().enumerate() {
            write!(
                layout,
                " tab name={} bkpr_workspace_id={} bkpr_workspace_name={}",
                quote(&tab.name)?,
                quote(&workspace_id)?,
                quote(&workspace.name)?,
            )
            .unwrap();
            if tab_index == focused_tab {
                layout.push_str(" focus=true");
            }
            tab_index += 1;
            layout.push_str(" {\n");
            let mut state = LeafState {
                prefix: format!("w{w}t{t}"),
                next: 0,
                focus_taken: false,
                leaf,
            };
            write_node(&mut layout, &tab.layout, 2, &mut state)?;
            layout.push_str(" }\n");
        }
    }
    layout.push_str("}\n");
    Ok(layout)
}

struct LeafState<'a> {
    prefix: String,
    next: usize,
    focus_taken: bool,
    leaf: &'a mut dyn FnMut(&str, &RecoveryPane) -> RestoreLeaf,
}

fn write_node(
    out: &mut String,
    node: &SplitNode<RecoveryPane>,
    depth: usize,
    state: &mut LeafState<'_>,
) -> Result<(), serde_json::Error> {
    let indent = " ".repeat(depth);
    match node {
        SplitNode::Pane { pane } => write_leaf(out, pane, depth, state),
        SplitNode::Split {
            direction,
            children,
        } => {
            writeln!(
                out,
                "{indent}pane split_direction=\"{}\" {{",
                direction.kdl()
            )
            .unwrap();
            for child in children {
                write_node(out, child, depth + 1, state)?;
            }
            writeln!(out, "{indent}}}").unwrap();
            Ok(())
        }
        SplitNode::Stack { panes } => {
            writeln!(out, "{indent}pane stacked=true {{").unwrap();
            for pane in panes {
                write_leaf(out, pane, depth + 1, state)?;
            }
            writeln!(out, "{indent}}}").unwrap();
            Ok(())
        }
    }
}

fn write_leaf(
    out: &mut String,
    pane: &RecoveryPane,
    depth: usize,
    state: &mut LeafState<'_>,
) -> Result<(), serde_json::Error> {
    let quote = |value: &str| serde_json::to_string(value);
    let indent = " ".repeat(depth);
    let key = format!("{}p{}", state.prefix, state.next);
    state.next += 1;
    let restore = (state.leaf)(&key, pane);
    write!(
        out,
        "{indent}pane cwd={} command={} close_on_exit=true",
        quote(&restore.cwd)?,
        quote(&restore.program)?,
    )
    .unwrap();
    if pane.focused && !state.focus_taken {
        state.focus_taken = true;
        out.push_str(" focus=true");
    }
    if restore.args.is_empty() {
        out.push('\n');
        return Ok(());
    }
    out.push_str(" {\n");
    write!(out, "{indent} args").unwrap();
    for arg in &restore.args {
        write!(out, " {}", quote(arg)?).unwrap();
    }
    write!(out, "\n{indent}}}\n").unwrap();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(name: &str, focused: bool) -> RecoveryPane {
        RecoveryPane {
            cwd: Some(format!("/work/{name}")),
            name: name.into(),
            renamed: false,
            harness: None,
            agent_id: None,
            focused,
        }
    }

    fn leaf_node(name: &str, focused: bool) -> SplitNode<RecoveryPane> {
        SplitNode::Pane {
            pane: pane(name, focused),
        }
    }

    fn record(workspaces: Vec<RecoveryWorkspace>) -> RecoveryRecord {
        RecoveryRecord {
            version: RECOVERY_VERSION,
            session_name: "dev".into(),
            session_id: "sid".into(),
            sequence: 7,
            written_by: "0.0.0".into(),
            updated_at_millis: 1,
            workspaces,
        }
    }

    fn geo(id: u32, x: usize, y: usize, columns: usize, rows: usize) -> GeometryPane {
        GeometryPane {
            id,
            x,
            y,
            columns,
            rows,
        }
    }

    fn restore(key: &str, pane: &RecoveryPane) -> RestoreLeaf {
        RestoreLeaf {
            key: key.into(),
            cwd: pane.cwd.clone().unwrap_or_default(),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "true".into()],
        }
    }

    fn layout_of(record: &RecoveryRecord) -> String {
        recovery_layout(
            "file:/bkpr.wasm",
            &BTreeMap::new(),
            12,
            "sid",
            record,
            &mut restore,
        )
        .unwrap()
    }

    #[test]
    fn record_roundtrips_and_rejects_future_version() {
        let original = record(vec![RecoveryWorkspace {
            name: "ws".into(),
            tabs: vec![RecoveryTab {
                name: "t".into(),
                focused: true,
                layout: leaf_node("a", true),
            }],
        }]);
        let bytes = serde_json::to_vec(&original).unwrap();
        assert_eq!(RecoveryRecord::decode(&bytes).unwrap(), original);
        assert!(original.usable());
        let mut future: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        future["version"] = 2.into();
        let error = RecoveryRecord::decode(&serde_json::to_vec(&future).unwrap()).unwrap_err();
        assert!(matches!(error, RecoveryDecodeError::FutureVersion(2)));
        assert!(!record(vec![]).usable());
    }

    #[test]
    fn split_tree_reconstructs_nested_guillotine() {
        let tree = split_tree(&[
            geo(2, 40, 10, 40, 10),
            geo(0, 0, 0, 40, 20),
            geo(1, 40, 0, 40, 10),
        ])
        .unwrap();
        assert_eq!(
            tree,
            SplitNode::Split {
                direction: SplitDirection::Vertical,
                children: vec![
                    SplitNode::Pane { pane: 0 },
                    SplitNode::Split {
                        direction: SplitDirection::Horizontal,
                        children: vec![SplitNode::Pane { pane: 1 }, SplitNode::Pane { pane: 2 }],
                    },
                ],
            }
        );
    }

    #[test]
    fn split_tree_detects_stack() {
        let tree = split_tree(&[
            geo(3, 40, 2, 40, 18),
            geo(1, 40, 0, 40, 1),
            geo(2, 40, 1, 40, 1),
        ])
        .unwrap();
        assert_eq!(
            tree,
            SplitNode::Stack {
                panes: vec![1, 2, 3]
            }
        );
    }

    /// The pinned runtime parks hidden stack members as suppressed panes resized to the
    /// visible member's full rectangle (no 1-row collapsed members): shared rectangles
    /// are one stack, in id order, wherever the stack sits in the split tree.
    #[test]
    fn split_tree_groups_shared_rectangles_into_stacks() {
        assert_eq!(
            split_tree(&[
                geo(2, 40, 0, 160, 50),
                geo(0, 40, 0, 160, 50),
                geo(1, 40, 0, 160, 50),
            ])
            .unwrap(),
            SplitNode::Stack {
                panes: vec![0, 1, 2]
            }
        );
        assert_eq!(
            split_tree(&[
                geo(7, 0, 0, 40, 50),
                geo(9, 40, 25, 40, 25),
                geo(3, 40, 0, 40, 25),
                geo(8, 40, 25, 40, 25),
            ])
            .unwrap(),
            SplitNode::Split {
                direction: SplitDirection::Vertical,
                children: vec![
                    SplitNode::Pane { pane: 7 },
                    SplitNode::Split {
                        direction: SplitDirection::Horizontal,
                        children: vec![
                            SplitNode::Pane { pane: 3 },
                            SplitNode::Stack { panes: vec![8, 9] },
                        ],
                    },
                ],
            }
        );
    }

    #[test]
    fn split_tree_ignores_non_tiled_and_degrades() {
        let tree = split_tree(&[geo(5, 0, 0, 30, 10), geo(4, 10, 5, 30, 10)]).unwrap();
        assert_eq!(
            tree,
            SplitNode::Split {
                direction: SplitDirection::Vertical,
                children: vec![SplitNode::Pane { pane: 4 }, SplitNode::Pane { pane: 5 }],
            }
        );
        assert_eq!(split_tree(&[]), None);
    }

    #[test]
    fn layout_emits_tabs_workspaces_and_no_sizes() {
        let two = SplitNode::Split {
            direction: SplitDirection::Horizontal,
            children: vec![leaf_node("b", false), leaf_node("c", true)],
        };
        let tab = |name: &str, focused, layout| RecoveryTab {
            name: name.into(),
            focused,
            layout,
        };
        let record = record(vec![
            RecoveryWorkspace {
                name: "first".into(),
                tabs: vec![tab("one", false, leaf_node("a", false))],
            },
            RecoveryWorkspace {
                name: "second".into(),
                tabs: vec![
                    tab("two", true, two),
                    tab(
                        "three",
                        false,
                        SplitNode::Stack {
                            panes: vec![pane("d", false), pane("e", false)],
                        },
                    ),
                ],
            },
        ]);
        let mut keys = Vec::new();
        let layout = recovery_layout(
            "file:/bkpr.wasm",
            &BTreeMap::new(),
            12,
            "sid",
            &record,
            &mut |key, pane| {
                keys.push(key.to_string());
                restore(key, pane)
            },
        )
        .unwrap();
        assert_eq!(keys, ["w0t0p0", "w1t0p0", "w1t0p1", "w1t1p0", "w1t1p1"]);
        assert_eq!(layout.matches("\n tab ").count(), 3);
        assert!(layout.contains("bkpr_workspace_id=\"sid:workspace:bkpr:00000\""));
        assert!(layout.contains("bkpr_workspace_id=\"sid:workspace:bkpr:00001\""));
        assert!(layout.contains("bkpr_workspace_name=\"second\""));
        assert_eq!(layout.matches("tab name=").count(), 3);
        assert_eq!(
            layout
                .lines()
                .filter(|line| line.starts_with(" tab ") && line.contains("focus=true"))
                .count(),
            1
        );
        assert!(layout.contains(
            "pane cwd=\"/work/c\" command=\"/bin/sh\" close_on_exit=true focus=true {\n    args \"-c\" \"true\"\n   }"
        ));
        assert_eq!(layout.matches("focus=true").count(), 2);
        assert!(layout.contains("pane split_direction=\"horizontal\" {"));
        assert!(layout.contains("pane stacked=true {"));
        assert_eq!(layout.matches("size=").count(), 1);
        assert!(layout.contains("size=\"12%\""));
    }

    #[test]
    fn layout_quotes_hostile_names_and_paths() {
        let hostile_name = "t\"a\\b{c}";
        let hostile_cwd = "/w\"x\\y{z";
        let mut hostile = pane("a", true);
        hostile.cwd = Some(hostile_cwd.into());
        let record = record(vec![RecoveryWorkspace {
            name: hostile_name.into(),
            tabs: vec![RecoveryTab {
                name: hostile_name.into(),
                focused: true,
                layout: SplitNode::Pane { pane: hostile },
            }],
        }]);
        let layout = layout_of(&record);
        for value in [hostile_name, hostile_cwd] {
            let quoted = serde_json::to_string(value).unwrap();
            assert!(layout.contains(&quoted), "{quoted} missing from {layout}");
            assert_eq!(serde_json::from_str::<String>(&quoted).unwrap(), value);
        }
        // No raw (unescaped) hostile text can terminate a string early.
        assert!(!layout.contains("cwd=\"/w\"x"));
    }

    #[test]
    fn restored_workspace_ids_sort_before_allocated() {
        let sid = "sid";
        assert!(restored_workspace_id(sid, 9) < format!("{sid}:workspace:bkpr:1"));
        assert_eq!(restored_workspace_id(sid, 1), "sid:workspace:bkpr:00001");
    }
}
