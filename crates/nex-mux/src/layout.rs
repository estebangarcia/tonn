//! Binary tree layout for terminal pane splits.

use std::collections::HashMap;

use nex_common::PaneId;
use serde::{Deserialize, Serialize};

use crate::Rect;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum SplitDirection {
    /// Left | Right (divider is a vertical line).
    Vertical,
    /// Top / Bottom (divider is a horizontal line).
    Horizontal,
}

/// Binary tree of pane splits.
pub enum LayoutNode {
    Leaf {
        pane_id: PaneId,
    },
    Split {
        direction: SplitDirection,
        /// Fraction (0.0–1.0) of space allocated to `first`.
        ratio: f32,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

const DIVIDER_PX: f32 = 2.0;
const MIN_SPLIT_RATIO: f32 = 0.1;
const MAX_SPLIT_RATIO: f32 = 0.9;

impl LayoutNode {
    /// Recursively assign pixel bounds to each leaf pane.
    pub fn compute_bounds(&self, available: Rect, out: &mut HashMap<PaneId, Rect>) {
        match self {
            LayoutNode::Leaf { pane_id } => {
                out.insert(*pane_id, available);
            }
            LayoutNode::Split { direction, ratio, first, second } => {
                let (first_rect, second_rect) = match direction {
                    SplitDirection::Vertical => {
                        let split = available.width * ratio;
                        let half_div = DIVIDER_PX / 2.0;
                        (
                            Rect {
                                x: available.x,
                                y: available.y,
                                width: (split - half_div).max(0.0),
                                height: available.height,
                            },
                            Rect {
                                x: available.x + split + half_div,
                                y: available.y,
                                width: (available.width - split - half_div).max(0.0),
                                height: available.height,
                            },
                        )
                    }
                    SplitDirection::Horizontal => {
                        let split = available.height * ratio;
                        let half_div = DIVIDER_PX / 2.0;
                        (
                            Rect {
                                x: available.x,
                                y: available.y,
                                width: available.width,
                                height: (split - half_div).max(0.0),
                            },
                            Rect {
                                x: available.x,
                                y: available.y + split + half_div,
                                width: available.width,
                                height: (available.height - split - half_div).max(0.0),
                            },
                        )
                    }
                };
                first.compute_bounds(first_rect, out);
                second.compute_bounds(second_rect, out);
            }
        }
    }

    /// Replace a leaf with a split containing the old leaf and a new pane.
    pub fn split_leaf(
        &mut self,
        target: PaneId,
        new_pane_id: PaneId,
        direction: SplitDirection,
    ) -> bool {
        match self {
            LayoutNode::Leaf { pane_id } if *pane_id == target => {
                let old = LayoutNode::Leaf { pane_id: target };
                let new = LayoutNode::Leaf { pane_id: new_pane_id };
                *self = LayoutNode::Split {
                    direction,
                    ratio: 0.5,
                    first: Box::new(old),
                    second: Box::new(new),
                };
                true
            }
            LayoutNode::Split { first, second, .. } => {
                first.split_leaf(target, new_pane_id, direction)
                    || second.split_leaf(target, new_pane_id, direction)
            }
            _ => false,
        }
    }

    /// Remove a leaf, collapsing the parent Split to the sibling.
    pub fn remove_leaf(&mut self, target: PaneId) -> bool {
        match self {
            LayoutNode::Split { first, second, .. } => {
                if matches!(**first, LayoutNode::Leaf { pane_id } if pane_id == target) {
                    // Replace self with second
                    let sibling = std::mem::replace(
                        second.as_mut(),
                        LayoutNode::Leaf { pane_id: PaneId::new() },
                    );
                    *self = sibling;
                    return true;
                }
                if matches!(**second, LayoutNode::Leaf { pane_id } if pane_id == target) {
                    let sibling = std::mem::replace(
                        first.as_mut(),
                        LayoutNode::Leaf { pane_id: PaneId::new() },
                    );
                    *self = sibling;
                    return true;
                }
                first.remove_leaf(target) || second.remove_leaf(target)
            }
            _ => false,
        }
    }

    /// Collect all leaf pane IDs in preorder traversal.
    pub fn pane_ids(&self) -> Vec<PaneId> {
        let mut ids = Vec::new();
        self.collect_pane_ids(&mut ids);
        ids
    }

    fn collect_pane_ids(&self, out: &mut Vec<PaneId>) {
        match self {
            LayoutNode::Leaf { pane_id } => out.push(*pane_id),
            LayoutNode::Split { first, second, .. } => {
                first.collect_pane_ids(out);
                second.collect_pane_ids(out);
            }
        }
    }

    /// Collect divider lines for rendering. Returns (direction, pixel_position, start, end).
    pub fn divider_lines(&self, bounds: Rect) -> Vec<DividerLine> {
        let mut lines = Vec::new();
        self.collect_dividers(bounds, &mut lines);
        lines
    }

    fn collect_dividers(&self, bounds: Rect, out: &mut Vec<DividerLine>) {
        if let LayoutNode::Split { direction, ratio, first, second } = self {
            match direction {
                SplitDirection::Vertical => {
                    let x = bounds.x + bounds.width * ratio;
                    out.push(DividerLine {
                        direction: *direction,
                        x,
                        y: bounds.y,
                        length: bounds.height,
                    });
                    let first_rect = Rect {
                        x: bounds.x, y: bounds.y,
                        width: bounds.width * ratio - DIVIDER_PX / 2.0,
                        height: bounds.height,
                    };
                    let second_rect = Rect {
                        x: x + DIVIDER_PX / 2.0, y: bounds.y,
                        width: bounds.width * (1.0 - ratio) - DIVIDER_PX / 2.0,
                        height: bounds.height,
                    };
                    first.collect_dividers(first_rect, out);
                    second.collect_dividers(second_rect, out);
                }
                SplitDirection::Horizontal => {
                    let y = bounds.y + bounds.height * ratio;
                    out.push(DividerLine {
                        direction: *direction,
                        x: bounds.x,
                        y,
                        length: bounds.width,
                    });
                    let first_rect = Rect {
                        x: bounds.x, y: bounds.y,
                        width: bounds.width,
                        height: bounds.height * ratio - DIVIDER_PX / 2.0,
                    };
                    let second_rect = Rect {
                        x: bounds.x, y: y + DIVIDER_PX / 2.0,
                        width: bounds.width,
                        height: bounds.height * (1.0 - ratio) - DIVIDER_PX / 2.0,
                    };
                    first.collect_dividers(first_rect, out);
                    second.collect_dividers(second_rect, out);
                }
            }
        }
    }

    /// Find a divider near a pixel position (for drag resize).
    /// Returns a mutable reference path if within threshold.
    pub fn adjust_ratio_at(&mut self, bounds: Rect, px: f32, py: f32, threshold: f32) -> bool {
        if let LayoutNode::Split { direction, ratio, first, second } = self {
            match direction {
                SplitDirection::Vertical => {
                    let divider_x = bounds.x + bounds.width * *ratio;
                    if (px - divider_x).abs() < threshold
                        && py >= bounds.y
                        && py <= bounds.y + bounds.height
                    {
                        *ratio = ((px - bounds.x) / bounds.width).clamp(MIN_SPLIT_RATIO, MAX_SPLIT_RATIO);
                        return true;
                    }
                    let first_bounds = Rect {
                        x: bounds.x, y: bounds.y,
                        width: bounds.width * *ratio,
                        height: bounds.height,
                    };
                    let second_bounds = Rect {
                        x: bounds.x + bounds.width * *ratio, y: bounds.y,
                        width: bounds.width * (1.0 - *ratio),
                        height: bounds.height,
                    };
                    first.adjust_ratio_at(first_bounds, px, py, threshold)
                        || second.adjust_ratio_at(second_bounds, px, py, threshold)
                }
                SplitDirection::Horizontal => {
                    let divider_y = bounds.y + bounds.height * *ratio;
                    if (py - divider_y).abs() < threshold
                        && px >= bounds.x
                        && px <= bounds.x + bounds.width
                    {
                        *ratio = ((py - bounds.y) / bounds.height).clamp(MIN_SPLIT_RATIO, MAX_SPLIT_RATIO);
                        return true;
                    }
                    let first_bounds = Rect {
                        x: bounds.x, y: bounds.y,
                        width: bounds.width,
                        height: bounds.height * *ratio,
                    };
                    let second_bounds = Rect {
                        x: bounds.x, y: bounds.y + bounds.height * *ratio,
                        width: bounds.width,
                        height: bounds.height * (1.0 - *ratio),
                    };
                    first.adjust_ratio_at(first_bounds, px, py, threshold)
                        || second.adjust_ratio_at(second_bounds, px, py, threshold)
                }
            }
        } else {
            false
        }
    }
}

/// A divider line for rendering between split panes.
pub struct DividerLine {
    pub direction: SplitDirection,
    pub x: f32,
    pub y: f32,
    pub length: f32,
}
