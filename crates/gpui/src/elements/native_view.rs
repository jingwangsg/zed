use crate::{Bounds, Hitbox, HitboxBehavior, IntoElement, Pixels, Styled, canvas, point, px, size};
use std::rc::Rc;

/// A platform child view whose visible and interactive regions follow GPUI's hitboxes.
pub trait PlatformNativeView {
    /// Apply window-relative logical-pixel bounds; only the supplied regions may draw or
    /// receive input.
    fn set_frame(&self, bounds: Bounds<Pixels>, visible_regions: &[Bounds<Pixels>]);
    /// Remove the view from both painting and hit testing when it leaves the element tree.
    fn hide(&self);
}

/// Embed a native child view with GPUI clipping, occlusion, and cached-frame replay.
pub fn native_view(view: Rc<dyn PlatformNativeView>) -> impl IntoElement + Styled {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::BlockMouse),
        move |_, hitbox, window, _| window.paint_native_view(view, hitbox),
    )
}

#[derive(Clone)]
pub(crate) struct NativeViewPlacement {
    pub view: Rc<dyn PlatformNativeView>,
    pub hitbox: Hitbox,
}

/// Subtract opaque hitboxes into non-overlapping rectangles. Overlapping popovers
/// must not expose a hole again, as an even-odd mask of their rectangles would.
pub(crate) fn native_view_regions(
    hitbox: &Hitbox,
    later_hitboxes: &[Hitbox],
) -> Vec<Bounds<Pixels>> {
    let mut regions = vec![hitbox.bounds.intersect(&hitbox.content_mask.bounds)];
    for overlay in later_hitboxes
        .iter()
        .filter(|hitbox| hitbox.behavior != HitboxBehavior::Normal)
    {
        let covered = overlay.bounds.intersect(&overlay.content_mask.bounds);
        regions = regions
            .into_iter()
            .flat_map(|region| {
                let intersection = region.intersect(&covered);
                if intersection.size.width <= px(0.) || intersection.size.height <= px(0.) {
                    return vec![region];
                }
                let right = region.origin.x + region.size.width;
                let bottom = region.origin.y + region.size.height;
                let cut_right = intersection.origin.x + intersection.size.width;
                let cut_bottom = intersection.origin.y + intersection.size.height;
                [
                    Bounds::new(
                        region.origin,
                        size(region.size.width, intersection.origin.y - region.origin.y),
                    ),
                    Bounds::new(
                        point(region.origin.x, cut_bottom),
                        size(region.size.width, bottom - cut_bottom),
                    ),
                    Bounds::new(
                        point(region.origin.x, intersection.origin.y),
                        size(
                            intersection.origin.x - region.origin.x,
                            intersection.size.height,
                        ),
                    ),
                    Bounds::new(
                        point(cut_right, intersection.origin.y),
                        size(right - cut_right, intersection.size.height),
                    ),
                ]
                .into_iter()
                .filter(|bounds| bounds.size.width > px(0.) && bounds.size.height > px(0.))
                .collect()
            })
            .collect();
    }
    regions.retain(|bounds| bounds.size.width > px(0.) && bounds.size.height > px(0.));
    regions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContentMask, HitboxId};

    #[test]
    fn overlapping_overlays_remain_occluded() {
        let full = Bounds::new(point(px(0.), px(0.)), size(px(100.), px(100.)));
        let base = Hitbox {
            id: HitboxId::placeholder(),
            bounds: full,
            content_mask: ContentMask { bounds: full },
            behavior: HitboxBehavior::BlockMouse,
        };
        let overlay = |x, y| Hitbox {
            bounds: Bounds::new(point(px(x), px(y)), size(px(50.), px(50.))),
            ..base.clone()
        };
        let regions = native_view_regions(&base, &[overlay(10., 10.), overlay(30., 30.)]);
        for region in &regions {
            for overlay in [overlay(10., 10.), overlay(30., 30.)] {
                let overlap = region.intersect(&overlay.bounds);
                assert!(overlap.size.width <= px(0.) || overlap.size.height <= px(0.));
            }
        }
        let area: f32 = regions
            .iter()
            .map(|region| f32::from(region.size.width) * f32::from(region.size.height))
            .sum();
        assert_eq!(area, 5900.);
        assert!(native_view_regions(&base, std::slice::from_ref(&base)).is_empty());
    }
}
