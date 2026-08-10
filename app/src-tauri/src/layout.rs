// layout — parse tmux `window_layout` strings into per-pane rects.
//
// tmux is the layout authority (bug #10): the frontend mirrors whatever
// arrangement tmux produced instead of guessing with its own CSS grid. This
// module turns the layout string the window already reports into absolute
// cell-space rectangles the frontend can position panes from.
//
// Grammar (observed live on tmux 3.6b, matches layout-custom.c):
//   layout    := checksum "," node
//   node      := geom ( "," pane-num | container )
//   geom      := W "x" H "," X "," Y
//   container := "{" node ("," node)+ "}"     horizontal row of children
//              | "[" node ("," node)+ "]"     vertical column of children
// Leaf pane-num is the numeric part of the tmux pane ID (`%3` -> `3`), NOT the
// pane index — verified by killing a middle pane and re-splitting.

use serde::Serialize;

/// One pane's rectangle in tmux cell space (origin top-left, borders exclusive).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutRect {
    /// tmux pane id, e.g. "%3".
    pub pane_id: String,
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

/// Parsed window layout: total window size in cells + every leaf pane's rect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WindowLayout {
    pub width: u16,
    pub height: u16,
    pub rects: Vec<LayoutRect>,
}

/// Parse a tmux `#{window_layout}` string. Returns None on any malformed
/// input — the caller treats that as "no geometry known" and the frontend
/// falls back to its own grid, so a parse failure degrades, never breaks.
pub fn parse_window_layout(s: &str) -> Option<WindowLayout> {
    // Strip "cksum," prefix (4 hex chars).
    let rest = s.trim();
    let (cksum, rest) = rest.split_once(',')?;
    if cksum.len() != 4 || !cksum.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut p = Parser {
        bytes: rest.as_bytes(),
        pos: 0,
    };
    let mut rects = Vec::new();
    let (w, h) = p.node(&mut rects)?;
    if p.pos != p.bytes.len() {
        return None; // trailing garbage
    }
    Some(WindowLayout {
        width: w,
        height: h,
        rects,
    })
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn eat(&mut self, b: u8) -> Option<()> {
        if self.peek() == Some(b) {
            self.pos += 1;
            Some(())
        } else {
            None
        }
    }

    fn number(&mut self) -> Option<u16> {
        let start = self.pos;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.pos == start {
            return None;
        }
        std::str::from_utf8(&self.bytes[start..self.pos])
            .ok()?
            .parse()
            .ok()
    }

    /// Parse one node; append leaf rects; return the node's (w, h).
    fn node(&mut self, rects: &mut Vec<LayoutRect>) -> Option<(u16, u16)> {
        let w = self.number()?;
        self.eat(b'x')?;
        let h = self.number()?;
        self.eat(b',')?;
        let x = self.number()?;
        self.eat(b',')?;
        let y = self.number()?;
        match self.peek() {
            // Container: children carry absolute coords already, so x/y of the
            // container itself is only consumed, not propagated.
            Some(b'{') | Some(b'[') => {
                let close = if self.peek() == Some(b'{') { b'}' } else { b']' };
                self.pos += 1;
                self.node(rects)?;
                while self.eat(b',').is_some() {
                    self.node(rects)?;
                }
                self.eat(close)?;
            }
            // Leaf: ",<pane-num>".
            Some(b',') => {
                self.pos += 1;
                let id = self.number()?;
                rects.push(LayoutRect {
                    pane_id: format!("%{id}"),
                    x,
                    y,
                    w,
                    h,
                });
            }
            _ => return None,
        }
        Some((w, h))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // All fixture strings captured live from a tmux 3.6b probe socket.

    fn rect(id: &str, x: u16, y: u16, w: u16, h: u16) -> LayoutRect {
        LayoutRect {
            pane_id: id.into(),
            x,
            y,
            w,
            h,
        }
    }

    #[test]
    fn single_pane() {
        let l = parse_window_layout("a9ad,189x109,0,0,0").unwrap();
        assert_eq!((l.width, l.height), (189, 109));
        assert_eq!(l.rects, vec![rect("%0", 0, 0, 189, 109)]);
    }

    #[test]
    fn two_panes_vertical() {
        let l = parse_window_layout("4d2b,189x109,0,0[189x54,0,0,0,189x54,0,55,1]").unwrap();
        assert_eq!((l.width, l.height), (189, 109));
        assert_eq!(
            l.rects,
            vec![rect("%0", 0, 0, 189, 54), rect("%1", 0, 55, 189, 54)]
        );
    }

    #[test]
    fn three_panes_tiled_bottom_full_width() {
        // The exact shape that garbled under the CSS grid: bottom pane is 189
        // wide while the CSS cell was 94.
        let l = parse_window_layout(
            "24f8,189x109,0,0[189x54,0,0{94x54,0,0,0,94x54,95,0,1},189x54,0,55,2]",
        )
        .unwrap();
        assert_eq!(
            l.rects,
            vec![
                rect("%0", 0, 0, 94, 54),
                rect("%1", 95, 0, 94, 54),
                rect("%2", 0, 55, 189, 54),
            ]
        );
    }

    #[test]
    fn five_panes_tiled_two_plus_two_plus_one() {
        let l = parse_window_layout(
            "2d1f,189x109,0,0[189x35,0,0{94x35,0,0,0,94x35,95,0,1},189x35,0,36{94x35,0,36,2,94x35,95,36,3},189x37,0,72,4]",
        )
        .unwrap();
        assert_eq!(l.rects.len(), 5);
        assert_eq!(l.rects[4], rect("%4", 0, 72, 189, 37));
        // Distinct y values = 3 pane-rows (the toolbar-budget input).
        let mut ys: Vec<u16> = l.rects.iter().map(|r| r.y).collect();
        ys.sort_unstable();
        ys.dedup();
        assert_eq!(ys, vec![0, 36, 72]);
    }

    #[test]
    fn even_horizontal_row() {
        let l = parse_window_layout(
            "0303,100x40,0,0{20x40,0,0,0,19x40,21,0,1,19x40,41,0,2,19x40,61,0,3,19x40,81,0,4}",
        )
        .unwrap();
        assert_eq!(l.rects.len(), 5);
        assert_eq!(l.rects[0], rect("%0", 0, 0, 20, 40));
        assert_eq!(l.rects[4], rect("%4", 81, 0, 19, 40));
    }

    #[test]
    fn pane_numbers_are_ids_not_indices() {
        // Captured after killing %1 and splitting again: leaves are 0, 2, 3.
        let l = parse_window_layout(
            "cc97,100x40,0,0[100x19,0,0{49x19,0,0,0,50x19,50,0,2},100x20,0,20,3]",
        )
        .unwrap();
        let ids: Vec<&str> = l.rects.iter().map(|r| r.pane_id.as_str()).collect();
        assert_eq!(ids, vec!["%0", "%2", "%3"]);
    }

    #[test]
    fn manual_split_left_full_height_right_stacked() {
        // A user's ⌘D then ⌘⇧D: left pane spans the full height while the
        // right half is stacked — the arrangement resize-only pushes preserve.
        // The left pane crosses the y=21 boundary (no re-tile normalizes it);
        // the frontend's edge math must absorb that row's chrome, so the rect
        // must keep the full 40-cell height.
        let l = parse_window_layout(
            "b1c2,100x40,0,0{49x40,0,0,0,50x40,50,0[50x19,50,0,1,50x20,50,20,2]}",
        )
        .unwrap();
        assert_eq!(
            l.rects,
            vec![
                rect("%0", 0, 0, 49, 40),
                rect("%1", 50, 0, 50, 19),
                rect("%2", 50, 20, 50, 20),
            ]
        );
    }

    #[test]
    fn malformed_inputs_return_none() {
        for bad in [
            "",
            "a9ad",
            "a9ad,",
            "not-a-layout",
            "zzzz,189x109,0,0,0",            // bad checksum chars
            "a9ad,189x109,0,0",              // geom with no leaf/container
            "a9ad,189x109,0,0[189x54,0,0,0", // unclosed container
            "a9ad,189x109,0,0,0trailing",    // trailing garbage
            "a9adx,189x109,0,0,0",           // 5-char checksum
        ] {
            assert!(parse_window_layout(bad).is_none(), "accepted: {bad}");
        }
    }

    #[test]
    fn nested_depth_and_empty_container_rejected() {
        assert!(parse_window_layout("a9ad,10x10,0,0[]").is_none());
        // Deep nesting parses fine (row inside column inside row).
        let l = parse_window_layout(
            "abcd,100x40,0,0{50x40,0,0[50x19,0,0,0,50x20,0,20{24x20,0,20,1,25x20,25,20,2}],49x40,51,0,3}",
        )
        .unwrap();
        assert_eq!(l.rects.len(), 4);
    }
}
