//! Where to open the popup relative to the tray icon, in physical screen pixels.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    Bottom,
    Top,
    Left,
    Right,
}

/// Top-left corner for a `width × height` popup next to `anchor`, kept inside the
/// monitor's work area. The taskbar edge is inferred from where the work area is inset;
/// with an auto-hidden taskbar, the anchor's nearest edge is used.
pub fn popup_origin(
    anchor: (i32, i32),
    monitor: Rect,
    work: Rect,
    (width, height): (i32, i32),
    margin: i32,
) -> (i32, i32) {
    let edge = if work.bottom < monitor.bottom {
        Edge::Bottom
    } else if work.top > monitor.top {
        Edge::Top
    } else if work.left > monitor.left {
        Edge::Left
    } else if work.right < monitor.right {
        Edge::Right
    } else {
        let distances = [
            (monitor.bottom - anchor.1, Edge::Bottom),
            (anchor.1 - monitor.top, Edge::Top),
            (anchor.0 - monitor.left, Edge::Left),
            (monitor.right - anchor.0, Edge::Right),
        ];
        distances
            .into_iter()
            .min_by_key(|(distance, _)| *distance)
            .map(|(_, edge)| edge)
            .unwrap_or(Edge::Bottom)
    };
    let (x, y) = match edge {
        Edge::Bottom => (anchor.0 - width / 2, work.bottom - height - margin),
        Edge::Top => (anchor.0 - width / 2, work.top + margin),
        Edge::Left => (work.left + margin, anchor.1 - height / 2),
        Edge::Right => (work.right - width - margin, anchor.1 - height / 2),
    };
    let clamp = |value: i32, low: i32, high: i32| value.min(high).max(low);
    (
        clamp(x, work.left + margin, work.right - width - margin),
        clamp(y, work.top + margin, work.bottom - height - margin),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITOR: Rect = Rect {
        left: 0,
        top: 0,
        right: 2560,
        bottom: 1440,
    };

    fn work(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn bottom_taskbar_opens_above_the_icon_and_stays_on_screen() {
        let work = work(0, 0, 2560, 1392);
        assert_eq!(
            popup_origin((2000, 1416), MONITOR, work, (510, 600), 12),
            (1745, 780)
        );
        // An icon at the far right edge pushes the popup inward.
        assert_eq!(
            popup_origin((2550, 1416), MONITOR, work, (510, 600), 12),
            (2038, 780)
        );
    }

    #[test]
    fn other_taskbar_edges_open_beside_the_taskbar() {
        assert_eq!(
            popup_origin((2000, 20), MONITOR, work(0, 48, 2560, 1440), (510, 600), 12),
            (1745, 60)
        );
        assert_eq!(
            popup_origin((20, 1300), MONITOR, work(48, 0, 2560, 1440), (510, 600), 12),
            (60, 828)
        );
        assert_eq!(
            popup_origin((2540, 700), MONITOR, work(0, 0, 2512, 1440), (510, 600), 12),
            (1990, 400)
        );
    }

    #[test]
    fn auto_hidden_taskbar_uses_the_anchor_edge() {
        assert_eq!(
            popup_origin((2000, 1435), MONITOR, MONITOR, (510, 600), 12),
            (1745, 828)
        );
    }

    #[test]
    fn secondary_monitor_coordinates_are_respected() {
        let monitor = Rect {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1080,
        };
        let work = work(-1920, 0, 0, 1040);
        assert_eq!(
            popup_origin((-100, 1060), monitor, work, (340, 400), 8),
            (-348, 632)
        );
    }
}
