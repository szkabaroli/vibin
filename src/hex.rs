//! Read-only hex viewer for binary files, with a "smart preview" tree for
//! well-known formats: sections are parsed into named byte ranges shown in
//! a hierarchy next to the dump. Selecting a node jumps to and highlights
//! its bytes. Currently WebAssembly (.wasm) modules are recognized.

use std::path::{Path, PathBuf};

/// One node of the structure tree: a named byte range in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HexNode {
    pub name: String,
    /// C-like type shown in the Type column, e.g. "u32" or "functype[4]".
    pub ty: String,
    /// The Value column: decoded scalars ("1 (0x1)") or "{ ... }".
    pub detail: String,
    /// Byte range in the file, end exclusive.
    pub start: usize,
    pub end: usize,
    /// Indentation level in the tree (0 = root).
    pub depth: usize,
}

/// Which half of the viewer has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HexFocus {
    Tree,
    Dump,
}

#[derive(Debug)]
pub struct HexView {
    pub path: PathBuf,
    pub data: Vec<u8>,
    /// Dump scroll position, in rows of `bytes_per_row`.
    pub scroll: usize,
    /// Flattened structure tree; empty for unrecognized formats.
    pub nodes: Vec<HexNode>,
    pub selected: usize,
    /// First visible tree row; kept tracking `selected` by the renderer.
    pub tree_scroll: usize,
    pub focus: HexFocus,
    /// Set by the renderer each frame, used by scroll clamping and paging.
    pub bytes_per_row: usize,
    pub viewport_rows: usize,
}

impl HexView {
    pub fn from_data(path: &Path, data: Vec<u8>) -> Self {
        let nodes = parse_structure(&data);
        let focus = if nodes.is_empty() { HexFocus::Dump } else { HexFocus::Tree };
        Self {
            path: path.to_path_buf(),
            data,
            scroll: 0,
            nodes,
            selected: 0,
            tree_scroll: 0,
            focus,
            bytes_per_row: 16,
            viewport_rows: 1,
        }
    }

    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    pub fn total_rows(&self) -> usize {
        self.data.len().div_ceil(self.bytes_per_row.max(1))
    }

    pub fn scroll_by(&mut self, delta: isize) {
        let max = self.total_rows().saturating_sub(self.viewport_rows);
        self.scroll = if delta < 0 {
            self.scroll.saturating_sub(delta.unsigned_abs())
        } else {
            self.scroll.saturating_add(delta as usize).min(max)
        };
    }

    /// The byte range of the selected tree node, for highlighting.
    pub fn selected_range(&self) -> Option<(usize, usize)> {
        let node = self.nodes.get(self.selected)?;
        // the root covers the whole file — highlighting it is just noise
        (node.depth > 0).then_some((node.start, node.end))
    }

    pub fn select_node(&mut self, index: usize) {
        if index >= self.nodes.len() {
            return;
        }
        self.selected = index;
        // bring the node's first byte into view, a row of context above
        let row = self.nodes[index].start / self.bytes_per_row.max(1);
        let last_visible = self.scroll + self.viewport_rows.saturating_sub(1);
        if row < self.scroll || row > last_visible {
            self.scroll = row.saturating_sub(1);
        }
    }

    pub fn select_next(&mut self) {
        self.select_node((self.selected + 1).min(self.nodes.len().saturating_sub(1)));
    }

    pub fn select_prev(&mut self) {
        self.select_node(self.selected.saturating_sub(1));
    }

    /// Innermost (deepest) node covering `offset`, skipping the root —
    /// drives the per-pattern coloring of the dump.
    pub fn covering_node(&self, offset: usize) -> Option<usize> {
        let mut best: Option<usize> = None;
        for (i, node) in self.nodes.iter().enumerate() {
            if node.depth > 0
                && offset >= node.start
                && offset < node.end
                && best.is_none_or(|b: usize| self.nodes[b].depth <= node.depth)
            {
                best = Some(i);
            }
        }
        best
    }

    /// Whether the node at `index` is the last among its siblings — drives
    /// the └╴ vs ├╴ connector in the tree rendering.
    pub fn is_last_sibling(&self, index: usize) -> bool {
        let depth = self.nodes[index].depth;
        for node in &self.nodes[index + 1..] {
            if node.depth < depth {
                return true;
            }
            if node.depth == depth {
                return false;
            }
        }
        true
    }
}

/// Human size: "18 B", "1.2 KB", "3.4 MB".
pub fn human_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Dispatch on magic bytes; empty result means "no smart preview".
/// All known formats live in assets/patterns/*.pat — see src/pattern.rs.
fn parse_structure(data: &[u8]) -> Vec<HexNode> {
    crate::pattern::match_and_evaluate(data).unwrap_or_default()
}


#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal valid module: header, one type, export "run", empty code.
    fn tiny_wasm() -> Vec<u8> {
        let mut b: Vec<u8> = b"\0asm\x01\0\0\0".to_vec();
        // type section: 1 entry, () -> ()
        b.extend_from_slice(&[1, 4, 1, 0x60, 0, 0]);
        // custom section named "name" with 2 payload bytes after the name
        b.extend_from_slice(&[0, 7, 4, b'n', b'a', b'm', b'e', 9, 9]);
        // export section: 1 entry "run" func 0
        b.extend_from_slice(&[7, 7, 1, 3, b'r', b'u', b'n', 0, 0]);
        b
    }

    #[test]
    fn wasm_pattern_names_sections_and_decodes_entries() {
        let data = tiny_wasm();
        let nodes = parse_structure(&data);
        assert_eq!(nodes[0].name, "wasm");
        assert_eq!((nodes[0].start, nodes[0].end), (0, data.len()));
        // section elements are named after their id enum
        let section_names: Vec<&str> = nodes
            .iter()
            .filter(|n| n.ty == "struct wasm_section")
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(section_names, vec!["type", "custom", "export"]);
        // custom section name and export entry decode via lstr
        assert!(nodes.iter().any(|n| n.ty == "str" && n.detail == "\"name\""));
        assert!(nodes.iter().any(|n| n.ty == "str" && n.detail == "\"run\""));
        // sections tile the file: the sections array covers header..eof
        let sections = nodes.iter().find(|n| n.name == "sections").unwrap();
        assert_eq!(sections.ty, "wasm_section[3]");
        assert_eq!((sections.start, sections.end), (8, data.len()));
        // scalars decode with both bases shown
        let version = nodes.iter().find(|n| n.name == "version").unwrap();
        assert_eq!(version.detail, "1 (0x1)");
    }

    #[test]
    fn last_sibling_detection_for_connectors() {
        let node = |depth: usize| HexNode {
            name: String::new(),
            ty: String::new(),
            detail: String::new(),
            start: 0,
            end: 0,
            depth,
        };
        let mut view = HexView::from_data(Path::new("x.bin"), vec![0xff]);
        view.nodes = vec![node(0), node(1), node(2), node(2), node(1)];
        assert!(!view.is_last_sibling(1), "another depth-1 node follows");
        assert!(!view.is_last_sibling(2), "sibling at the same depth");
        assert!(view.is_last_sibling(3), "next node is shallower");
        assert!(view.is_last_sibling(4));
        assert!(view.is_last_sibling(0));
    }

    #[test]
    fn non_wasm_binary_has_no_tree() {
        assert!(parse_structure(&[0x42, 0x13, 0x00, 0x99, 1, 2, 3]).is_empty());
        assert!(parse_structure(b"\0as").is_empty()); // too short
    }

    #[test]
    fn truncated_wasm_does_not_panic_or_loop() {
        let mut data = tiny_wasm();
        data.truncate(12); // cut inside the type section
        let nodes = parse_structure(&data);
        assert!(!nodes.is_empty());
        assert!(nodes.iter().all(|n| n.end <= data.len()));
        // pathological: section with a huge declared size
        let bad = b"\0asm\x01\0\0\0\x01\xff\xff\xff\xff\x0f".to_vec();
        let nodes = parse_structure(&bad);
        assert!(nodes.len() >= 2);
        assert!(nodes.iter().all(|n| n.end <= bad.len()));
    }

    #[test]
    fn view_scroll_and_node_selection_jump() {
        let mut view = HexView::from_data(Path::new("m.wasm"), tiny_wasm());
        assert_eq!(view.focus, HexFocus::Tree);
        view.bytes_per_row = 8;
        view.viewport_rows = 2;
        assert_eq!(view.total_rows(), 4); // 31 bytes / 8 per row
        view.scroll_by(100);
        assert_eq!(view.scroll, 2); // clamped to rows - viewport
        view.scroll_by(-100);
        assert_eq!(view.scroll, 0);
        // selecting the export section scrolls its start row into view
        let export = view.nodes.iter().position(|n| n.name == "export").unwrap();
        view.select_node(export);
        let row = view.nodes[export].start / 8;
        assert!(view.scroll >= row.saturating_sub(1) && view.scroll <= row);
        // the root node produces no highlight range
        view.select_node(0);
        assert_eq!(view.selected_range(), None);
        view.select_node(export);
        assert!(view.selected_range().is_some());
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human_size(18), "18 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(2 * 1024 * 1024), "2.0 MB");
    }
}
