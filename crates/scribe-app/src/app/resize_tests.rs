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
