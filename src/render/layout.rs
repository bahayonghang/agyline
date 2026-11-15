use crate::collect::FrameData;
use crate::config::{EffectiveConfig, ItemId};
use crate::items::runtime::eval_single_line_mode_pill;
use crate::render::StyledLine;

pub fn layout_lines(
    lines: Vec<StyledLine>,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Vec<StyledLine> {
    if config.single_line && lines.len() > 1 {
        let secondary_has_items = lines.iter().skip(1).any(|line| !line.items.is_empty());
        let mut first = lines
            .into_iter()
            .next()
            .unwrap_or(StyledLine { items: Vec::new() });
        if secondary_has_items
            && !first.items.iter().any(|item| item.id == ItemId::Mode)
            && let Some(pill) = eval_single_line_mode_pill(frame, config)
        {
            first.items.push(pill);
        }
        return if first.items.is_empty() {
            Vec::new()
        } else {
            vec![first]
        };
    }

    lines
        .into_iter()
        .filter(|line| !line.items.is_empty())
        .collect()
}
