//! Regression guard for the frameless resize hit-testing. The interior MUST
//! NOT be a resize zone (that's what made the resize overlay eat tab /
//! settings-✕ clicks); edges/corners must map to the right direction. Pure,
//! so it runs every CI build and pins the geometry across window sizes.
use super::chrome::resize_dir_at;
use egui::{pos2, Rect, ResizeDirection as D};

fn win() -> Rect {
    Rect::from_min_max(pos2(0.0, 0.0), pos2(1000.0, 700.0))
}

#[test]
fn interior_is_never_a_resize_zone() {
    assert_eq!(resize_dir_at(pos2(500.0, 350.0), win(), 6.0, 12.0), None);
    // The exact tab position the old Foreground overlay was eating.
    assert_eq!(resize_dir_at(pos2(574.0, 48.0), win(), 6.0, 12.0), None);
}

#[test]
fn edges_map_to_their_direction() {
    assert_eq!(
        resize_dir_at(pos2(500.0, 1.0), win(), 6.0, 12.0),
        Some(D::North)
    );
    assert_eq!(
        resize_dir_at(pos2(500.0, 699.0), win(), 6.0, 12.0),
        Some(D::South)
    );
    assert_eq!(
        resize_dir_at(pos2(1.0, 350.0), win(), 6.0, 12.0),
        Some(D::West)
    );
    assert_eq!(
        resize_dir_at(pos2(999.0, 350.0), win(), 6.0, 12.0),
        Some(D::East)
    );
}

#[test]
fn corners_take_priority_over_edges() {
    assert_eq!(
        resize_dir_at(pos2(2.0, 2.0), win(), 6.0, 12.0),
        Some(D::NorthWest)
    );
    assert_eq!(
        resize_dir_at(pos2(998.0, 2.0), win(), 6.0, 12.0),
        Some(D::NorthEast)
    );
    assert_eq!(
        resize_dir_at(pos2(2.0, 698.0), win(), 6.0, 12.0),
        Some(D::SouthWest)
    );
    assert_eq!(
        resize_dir_at(pos2(998.0, 698.0), win(), 6.0, 12.0),
        Some(D::SouthEast)
    );
    // On the top edge but within the corner band of the left side → NW.
    assert_eq!(
        resize_dir_at(pos2(8.0, 1.0), win(), 6.0, 12.0),
        Some(D::NorthWest)
    );
}

#[test]
fn outside_the_window_is_none() {
    assert_eq!(resize_dir_at(pos2(-5.0, 350.0), win(), 6.0, 12.0), None);
    assert_eq!(resize_dir_at(pos2(500.0, 800.0), win(), 6.0, 12.0), None);
}

#[test]
fn resize_dir_at_kills_offset_and_boundary_mutants() {
    // The tests above all use a (0,0)-origin `win()`, under which
    // `p.x - rect.left()` == `p.x + rect.left()` (left=0) — so the offset
    // mutants (`- -> +`) are equivalent, and they never probe the exact-border
    // (`==0`) or single-corner-band points, so the guard (`< -> <=`) and the
    // corner (`|| -> &&`) mutants survive. A NON-zero-origin rect plus
    // exact-border and single-corner-band probes make each one diverge.
    let r = Rect::from_min_max(pos2(10.0, 20.0), pos2(110.0, 220.0));
    let (e, c) = (8.0, 12.0);

    // (A) West-edge interior: l = p.x - left = 5. `- -> +` -> l=25 -> None.
    assert_eq!(resize_dir_at(pos2(15.0, 120.0), r, e, c), Some(D::West));
    // (B) North-edge interior: t = p.y - top = 4. `- -> +` -> t=44 -> None.
    assert_eq!(resize_dir_at(pos2(60.0, 24.0), r, e, c), Some(D::North));
    // (C) Just LEFT of window (l=-5): outside-guard `|| -> &&` at the l/r pair
    //     would make `(l<0 && r<0)` false -> proceed -> West; orig short-circuits None.
    assert_eq!(resize_dir_at(pos2(5.0, 120.0), r, e, c), None);
    // (D) EXACT left border (l==0): `l < 0.0 -> <=` guards it to None.
    assert_eq!(resize_dir_at(pos2(10.0, 120.0), r, e, c), Some(D::West));
    // (E) EXACT right border (r==0): `r < 0.0 -> <=`/`== 0.0` -> None.
    assert_eq!(resize_dir_at(pos2(110.0, 120.0), r, e, c), Some(D::East));
    // (F) EXACT top border (t==0): `t < 0.0 -> <=`/`== 0.0` -> None.
    assert_eq!(resize_dir_at(pos2(60.0, 20.0), r, e, c), Some(D::North));
    // (G) EXACT bottom border (b==0): `b < 0.0 -> <=` -> None.
    assert_eq!(resize_dir_at(pos2(60.0, 220.0), r, e, c), Some(D::South));
    // (H) NE: north edge, RIGHT corner band, OUTSIDE right edge band (r=10 in (8,12]).
    //     Only `(n && ne)` is true; `|| -> &&` collapses it to North.
    assert_eq!(resize_dir_at(pos2(100.0, 24.0), r, e, c), Some(D::NorthEast));
    // (I) SW: bottom edge, LEFT corner band, outside left edge band (l=10 in (8,12]).
    assert_eq!(resize_dir_at(pos2(20.0, 214.0), r, e, c), Some(D::SouthWest));
    // (J) SE: bottom edge, RIGHT corner band, outside right edge band (r=10 in (8,12]).
    assert_eq!(resize_dir_at(pos2(100.0, 214.0), r, e, c), Some(D::SouthEast));
}
